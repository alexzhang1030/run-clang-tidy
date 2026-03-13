use std::{fs, path};

#[allow(unused_imports)]
use color_eyre::{eyre::eyre, eyre::WrapErr, Help};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::Deserialize;

pub mod cli;
pub mod cmd;
mod ctcache;
mod sarif;

mod globs;
mod resolve;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonModel {
    pub paths: Vec<String>,
    pub filter_post: Option<Vec<String>>,
    pub style: Option<path::PathBuf>,
}

enum Dump {
    Error { msg: String, path: path::PathBuf },
    Warning { msg: String, path: path::PathBuf },
}

struct RunArtifacts {
    dump: Option<Dump>,
    sarif: sarif::Report,
}

fn log_pretty() -> bool {
    // fancy logging using indicatif is only done for log level "info". when debugging we
    // do not use a progress bar, if info is not enabled at all ("quiet") then the progress
    // is also not shown
    !log::log_enabled!(log::Level::Debug) && log::log_enabled!(log::Level::Info)
}

fn should_reveal_progress(position: u64) -> bool {
    position == 0
}

struct LogStep(u8);

impl LogStep {
    fn new() -> LogStep {
        LogStep(1)
    }

    fn next(&mut self) -> String {
        // TODO: the actual number of steps could be determined by a macro?
        let str = format!(
            "{}",
            console::style(format!("[ {:1}/7 ]", self.0)).bold().dim()
        );
        self.0 += 1;
        if log_pretty() {
            str
        } else {
            "".to_string()
        }
    }
}

fn get_command(data: &cli::Data) -> eyre::Result<cmd::Runner> {
    let cmd_path = resolve::command(data)?;
    let mut cmd = cmd::Runner::new(&cmd_path);

    cmd.validate()
        .wrap_err(format!(
            "Failed to execute the specified command '{}'",
            cmd_path.display()
        ))
        .suggestion(format!(
            "Please make sure that the command '{}' exists or is in your search path",
            cmd_path.to_string_lossy()
        ))?;

    Ok(cmd)
}

fn place_tidy_file(
    file_and_root: Option<(path::PathBuf, path::PathBuf)>,
    step: &mut LogStep,
) -> eyre::Result<Option<path::PathBuf>> {
    if file_and_root.is_none() {
        // in case no tidy file has been specified there's nothing to do
        return Ok(None);
    }

    // the tidy file `src` should be copied to the destination directory `dst`
    let (src_file, dst_root) = file_and_root.unwrap();
    let mut dst_file = path::PathBuf::from(dst_root.as_path());
    // by adding the filename of the tidy file we get the final name of the destination file
    dst_file.push(".clang-tidy");

    // it may happen that there is already a .clang-tidy file at the destination folder, e.g.,
    // because the user placed it there while working with an editor supporting `clang-tidy`.
    // in such a case we provide feedback by comparing the file contents and abort with an error
    // if they do not match.
    if dst_file.exists() {
        let src_name = src_file.display();
        let dst_name = dst_file.display();

        log::warn!("Encountered existing tidy file {}", dst_name);

        let content_src =
            fs::read_to_string(&src_file).wrap_err(format!("Failed to read '{dst_name}'"))?;
        let content_dst = fs::read_to_string(dst_file.as_path())
            .wrap_err(format!("Failed to read '{dst_name}'"))
            .wrap_err("Error while trying to compare existing tidy file")
            .suggestion(format!(
                "Please delete or fix the existing tidy file {dst_name}"
            ))?;

        if content_src == content_dst {
            log::info!(
                "{} Existing tidy file matches {}, skipping placement",
                step.next(),
                src_name
            );
            return Ok(None);
        }

        return Err(eyre::eyre!(
            "Existing tidy file {} does not match provided tidy file {}",
            dst_name,
            src_name
        )
        .suggestion(format!(
            "Please either delete the file {dst_name} or align the contents with {src_name}"
        )));
    }

    log::info!(
        "{} Copying tidy file to {}",
        step.next(),
        console::style(dst_file.to_string_lossy()).bold(),
    );

    // no file found at destination, copy the provided tidy file
    let _ = fs::copy(&src_file, &dst_file)
        .wrap_err(format!(
            "Failed to copy tidy file to {}",
            dst_root.to_string_lossy(),
        ))
        .suggestion(format!(
            "Please check the permissions for the folder {}",
            dst_root.to_string_lossy()
        ))?;

    Ok(Some(dst_file))
}

fn setup_jobs(jobs: Option<u8>) -> eyre::Result<()> {
    // configure rayon to use the specified number of threads (globally)
    if let Some(jobs) = jobs {
        let jobs = if jobs == 0 { 1u8 } else { jobs };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs.into())
            .build_global();

        if let Err(err) = pool {
            return Err(err)
                .wrap_err(format!("Failed to create thread pool of size {jobs}"))
                .suggestion("Please try to decrease the number of jobs");
        }
    };
    Ok(())
}

pub fn run(data: cli::Data) -> eyre::Result<()> {
    let start = std::time::Instant::now();

    log::info!(" ");
    let mut step = LogStep::new();

    let tidy_and_root = resolve::tidy_and_root(&data)?;
    if let Some((tidy_file, _)) = &tidy_and_root {
        log::info!(
            "{} Found tidy file {}",
            step.next(),
            console::style(tidy_file.to_string_lossy()).bold(),
        );
    } else {
        // no tidy file specified, it'll be picked by `clang-tidy` itself as the first `.clang-tidy`
        // file that is encountered when walking all parent paths recursively.
        log::info!(
            "{} No tidy file specified, assuming .clang-tidy exists in the project tree",
            step.next()
        );
    }

    let build_root = resolve::build_root(&data)?;
    log::info!(
        "{} Using build root {}",
        step.next(),
        console::style(build_root.to_string_lossy()).bold(),
    );

    let candidates =
        globs::build_matchers_from(&data.json.paths, &data.json.root, "paths", &data.json.name)?;
    let filter_pre =
        globs::build_glob_set_from(&data.json.filter_pre, "preFilter", &data.json.name)?;
    let filter_post =
        globs::build_glob_set_from(&data.json.filter_post, "postFilter", &data.json.name)?;

    let (paths, filtered) = globs::match_paths(candidates, filter_pre, filter_post);
    let paths = paths.into_iter().map(|p| p.canonicalize().unwrap());

    let filtered = if filtered.is_empty() {
        "".to_string()
    } else {
        format!(" (filtered {} paths)", filtered.len())
    };

    log::info!(
        "{} Found {} files for the provided path patterns{}",
        step.next(),
        console::style(paths.len()).bold(),
        filtered
    );

    let cmd = get_command(&data)?;
    let cmd_path = match cmd.get_path().canonicalize() {
        Ok(path) => path,
        Err(_) => cmd.get_path(),
    };
    log::info!(
        "{} Found clang-tidy version {} using command {}",
        step.next(),
        console::style(cmd.get_version().unwrap()).bold(),
        console::style(cmd_path.to_string_lossy()).bold(),
    );

    let strip_root = if let Some((_, tidy_root)) = &tidy_and_root {
        Some(path::PathBuf::from(tidy_root.as_path()))
    } else {
        None
    };

    let tidy = place_tidy_file(tidy_and_root, &mut step)?;
    // binding for scope guard is not used, but an action needed when the variable goes out of scope
    let _tidy = scopeguard::guard(tidy, |path| {
        // ensure we delete the temporary tidy file at return or panic
        if let Some(path) = path {
            let str = format!("Cleaning up temporary file {}\n", path.to_string_lossy());
            let str = console::style(str).dim().italic();

            log::info!("\n{}", str);
            let _ = fs::remove_file(path);
        }
    });

    let paths: Vec<_> = paths.collect();
    let ctcache = ctcache::Context::new(
        !data.force,
        &build_root,
        cmd.get_path().as_path(),
        cmd.get_version().as_deref(),
    )?;
    if data.force {
        log::info!("{} Bypassing ctcache due to --force", step.next());
    } else if data.fix {
        log::warn!("ctcache is bypassed when --fix is enabled");
    } else if let Some(ctcache) = &ctcache {
        log::info!(
            "{} Using ctcache directory {}",
            step.next(),
            console::style(ctcache.cache_dir().to_string_lossy()).bold(),
        );
    }

    setup_jobs(data.jobs)?;
    log::info!("{} Executing clang-tidy ...\n", step.next(),);

    let pb = indicatif::ProgressBar::new(paths.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(if console::Term::stdout().size().1 > 80 {
            "{prefix:>12.cyan.bold} [{bar:26}] {pos}/{len} {wide_msg}"
        } else {
            "{prefix:>12.cyan.bold} [{bar:26}] {pos}/{len}"
        })
        .unwrap()
        .progress_chars("=> "),
    );
    pb.set_draw_target(indicatif::ProgressDrawTarget::hidden());

    if log_pretty() {
        pb.set_prefix("Running");
    }

    let (failures, warnings, sarif_report) = {
        let ctcache = ctcache.as_ref();
        let dump: Vec<_> = paths
            .into_par_iter()
            .map(|path| {
                let result = match ctcache {
                    Some(ctcache) => {
                        ctcache.run_or_execute(&cmd, &path, data.fix, data.ignore_warn)
                    }
                    None => cmd.run_tidy(&path, &build_root, data.fix, data.ignore_warn),
                };
                let strip_path = match &strip_root {
                    None => path.clone(),
                    Some(strip) => {
                        if let Ok(path) = path.strip_prefix(strip) {
                            path.to_path_buf()
                        } else {
                            path.clone()
                        }
                    }
                };

                let (prefix, style, diagnostics) = match result {
                    cmd::RunResult::Ok => ("Ok", console::Style::new().green().bold(), None),
                    cmd::RunResult::Err(details) => {
                        ("Error", console::Style::new().red().bold(), Some(details))
                    }
                    cmd::RunResult::Warn(details) => (
                        "Warning",
                        console::Style::new().color256(58).bold(),
                        Some(details),
                    ),
                };
                log_step(prefix, path.as_path(), &strip_root, &pb, style);

                let Some(details) = diagnostics else {
                    return RunArtifacts {
                        dump: None,
                        sarif: sarif::Report::default(),
                    };
                };

                let analysis = sarif::analyze(&details.diagnostic_text(), strip_root.as_deref());
                if !data.quiet {
                    let output = if data.raw {
                        match prefix {
                            "Error" => Some(details.error_message()),
                            "Warning" => Some(details.warning_message()),
                            _ => None,
                        }
                    } else {
                        analysis
                            .render(data.filter.as_ref())
                            .or_else(|| details.process_error.clone())
                    };

                    if let Some(output) = output {
                        if log_pretty() {
                            pb.println(output.trim_end());
                        } else {
                            eprintln!("{}", output.trim_end());
                        }
                    }
                }

                let dump = match prefix {
                    "Error" => Some(Dump::Error {
                        msg: if data.raw {
                            details.error_message()
                        } else {
                            details
                                .process_error
                                .clone()
                                .unwrap_or_else(|| "clang-tidy reported errors".to_string())
                        },
                        path: strip_path,
                    }),
                    "Warning" => Some(Dump::Warning {
                        msg: if data.raw {
                            details.warning_message()
                        } else {
                            "clang-tidy reported warnings".to_string()
                        },
                        path: strip_path,
                    }),
                    _ => None,
                };

                RunArtifacts {
                    dump,
                    sarif: analysis.report,
                }
            })
            .collect();

        let mut failures = Vec::with_capacity(dump.len());
        let mut warnings: Vec<_> = vec![];
        let mut sarif_report = sarif::Report::default();

        dump.into_iter().for_each(|item| {
            sarif_report.append(item.sarif);
            if let Some(dump) = item.dump {
                match dump {
                    Dump::Error { msg, path } => failures.push((path, msg)),
                    Dump::Warning { msg, path } => warnings.push((path, msg)),
                };
            }
        });
        (failures, warnings, sarif_report)
    };

    let duration = start.elapsed();
    if log_pretty() {
        pb.finish();

        println!(
            "{:>12} in {}",
            console::Style::new().green().bold().apply_to("Finished"),
            indicatif::HumanDuration(duration)
        );
    } else {
        log::info!("{} Finished in {:#?}", step.next(), duration);
    }

    if let Some(output) = &data.sarif_output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).wrap_err(format!(
                "Failed to create SARIF output directory {}",
                parent.to_string_lossy()
            ))?;
        }
        let file = fs::File::create(output).wrap_err(format!(
            "Failed to create SARIF output file {}",
            output.to_string_lossy()
        ))?;
        sarif_report.write_to(file).wrap_err(format!(
            "Failed to write SARIF output file {}",
            output.to_string_lossy()
        ))?;
        log::info!(
            "Wrote SARIF report to {}",
            console::style(output.to_string_lossy()).bold(),
        );
    }

    fn collect_dump(items: Vec<(path::PathBuf, String)>, style: console::Style) -> String {
        items
            .into_iter()
            .map(|result| {
                format!(
                    "{}\n{}",
                    style.apply_to(result.0.to_string_lossy()),
                    result.1,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    if !warnings.is_empty() {
        log::warn!(
            "\n\nWarnings have been issued for the following files:\n\n{} ",
            collect_dump(
                warnings,
                console::Style::new().white().bold().on_color256(58)
            )
            .trim_end()
        );
    }

    if !failures.is_empty() {
        Err(eyre::eyre!(format!(
            "Execution failed for the following files:\n{}\n ",
            collect_dump(failures, console::Style::new().white().bold().on_red()).trim_end()
        )))
    } else {
        Ok(())
    }
}

fn log_step(
    prefix: &str,
    path: &path::Path,
    strip_path: &Option<path::PathBuf>,
    progress: &indicatif::ProgressBar,
    style: console::Style,
) {
    // let style = console::Style::new().green().bold();
    let print_path = match strip_path {
        None => path,
        Some(strip) => {
            if let Ok(path) = path.strip_prefix(strip) {
                path
            } else {
                path
            }
        }
    };

    if log_pretty() {
        if should_reveal_progress(progress.position()) {
            progress.set_draw_target(indicatif::ProgressDrawTarget::stderr());
        }
        progress.println(format!(
            "{:>12} {}",
            style.apply_to(prefix),
            print_path.to_string_lossy(),
        ));
        progress.inc(1);
    } else {
        log::info!("  + {}", path.to_string_lossy());
    }
}

#[cfg(test)]
mod tests {
    use super::should_reveal_progress;

    #[test]
    fn progress_stays_hidden_until_first_result() {
        assert!(should_reveal_progress(0));
        assert!(!should_reveal_progress(1));
        assert!(!should_reveal_progress(7));
    }
}
