use std::{fs, io, path, process};

use crate::cli::{self, utils};

#[allow(unused_imports)]
use color_eyre::{eyre::eyre, eyre::WrapErr, Help};

fn resolve_tidy_file(data: &cli::Data) -> eyre::Result<eyre::Result<path::PathBuf>> {
    let tidy_json = match &data.json.tidy_file {
        None => None,
        Some(path) => {
            let mut full_path = path::PathBuf::from(data.json.root.as_path());
            full_path.push(path);
            // do not perform the validation for the 'tidyFile' yet since a valid override
            // might have been passed as parameter to the tool
            Some(full_path)
        }
    };

    let tidy = match tidy_json {
        None => match &data.tidy_file {
            // the following only leads to an error if no tidyRoot has been specified
            None => Err(eyre::eyre!(
                "Tidy file must either be specified as \
                command-line parameter or within the configuration file"
            )),
            // tidy defined as CLI parameter but not in the .json configuration file
            Some(s_cli) => Ok(path::PathBuf::from(s_cli.as_path()).canonicalize().unwrap()),
        },
        Some(s_cfg) => match &data.tidy_file {
            // tidy defined in the .json configuration file but not as CLI parameter
            None => {
                // perform the evaluation of the tidy parameter only once it is used
                let path = utils::file_with_name_or_ext(s_cfg.as_path(), ".clang-tidy")
                    .wrap_err("Invalid configuration for 'tidyFile'")
                    .suggestion(format!(
                        "Check the content of the field 'tidyFile' in {}.",
                        data.json.name
                    ))?;
                Ok(path.canonicalize().unwrap())
            }
            // tidy defined in both, the .json configuration file and as CLI parameter
            Some(s_cli) => {
                log::debug!(
                    "Override detected:\ntidy file '{}' \
                        specified in '{}' is overridden by the \
                        command-line parameter: '{}'\n",
                    s_cfg.to_string_lossy(),
                    data.json.name,
                    s_cli.as_path().to_string_lossy()
                );
                Ok(path::PathBuf::from(s_cli.as_path()).canonicalize().unwrap())
            }
        },
    };

    Ok(tidy)
}

pub fn tidy_and_root(data: &cli::Data) -> eyre::Result<Option<(path::PathBuf, path::PathBuf)>> {
    let tidy_file = resolve_tidy_file(data)?;
    let tidy_root = match &data.json.tidy_root {
        None => None,
        Some(path) => {
            let path = if path.is_absolute() {
                path::PathBuf::from(path.as_path())
            } else {
                let mut full_path = path::PathBuf::from(data.json.root.as_path());
                full_path.push(path);
                full_path
            };
            Some(
                utils::dir_or_err(path.as_path())
                    .wrap_err("Invalid configuration for 'tidyRoot'")
                    .suggestion(
                        "Please make sure that 'tidyRoot' is a valid \
                         directory and check the access permissions",
                    )?
                    .canonicalize()
                    .unwrap(),
            )
        }
    };

    if let Err(tidy_err) = tidy_file {
        match tidy_root {
            // scenario: no root folder and no tidy file specified, simply run clang-format
            // and assume that there is a .clang-format file in the root folder of all files
            None => Ok(None),
            // unsupported scenario: root specified but missing tidy file
            Some(_) => Err(tidy_err.wrap_err(
                "A valid tidy file must be specified for \
                     configurations with the field 'tidyRoot'",
            ))
            .suggestion(
                "Specify the tidy file using the command line \
                 parameter or the field 'tidyRoot' within the configuration file.",
            ),
        }
    } else {
        match tidy_root {
            // scenario: root folder and tidy file have been specified. it is necessary to copy
            // the tidy file to the root folder before executing clang-format
            Some(tidy_root) => Ok(Some((tidy_file.unwrap(), tidy_root))),
            // unsupported scenario: tidy file specified but missing root folder
            None => Err(eyre::eyre!("Missing root folder configuration",)
                .wrap_err(format!(
                    "Found tidy file '{}' but could not find root folder configuration",
                    tidy_file.unwrap().to_string_lossy()
                ))
                .suggestion("Please add the field 'tidyRoot' to your configuration file.")),
        }
    }
}

pub fn build_root(data: &cli::Data) -> eyre::Result<path::PathBuf> {
    let build_root_json = match &data.json.build_root {
        None => None,
        Some(path) => {
            let mut full_path = path::PathBuf::from(data.json.root.as_path());
            full_path.push(path);
            // do not perform the validation for the 'buildRoot' yet since a valid override
            // might have been passed as parameter to the tool
            Some(full_path)
        }
    };

    let build_root = match build_root_json {
        None => match &data.build_root {
            None => Err(eyre::eyre!(
                "Build root must either be specified as \
                command-line parameter or within the configuration file"
            )),
            Some(s_cli) => Ok(path::PathBuf::from(s_cli.as_path()).canonicalize().unwrap()),
        },
        Some(s_cfg) => match &data.build_root {
            // build root defined in the .json configuration file but not as CLI parameter
            None => {
                // perform the evaluation of the command line parameter only once it is used
                let path = utils::dir_or_err(s_cfg.as_path())
                    .wrap_err("Invalid configuration for 'buildRoot'")
                    .suggestion(format!(
                        "Check the content of the field 'buildRoot' in {}.",
                        data.json.name
                    ))?;
                Ok(path.canonicalize().unwrap())
            }
            // buildRoot defined in both, the .json configuration file and as CLI parameter
            Some(s_cli) => {
                log::debug!(
                    "Override detected:\ncompile-commands file '{}' \
                        specified in '{}' is overridden by the \
                        command-line parameter: '{}'\n",
                    s_cfg.to_string_lossy(),
                    data.json.name,
                    s_cli.as_path().to_string_lossy()
                );
                Ok(path::PathBuf::from(s_cli.as_path()).canonicalize().unwrap())
            }
        },
    };

    build_root
}

pub fn command(data: &cli::Data) -> eyre::Result<path::PathBuf> {
    let mut from_json = false;

    let cmd = match &data.json.command {
        None => match &data.command {
            // use default value if not specified in configuration file nor as parameter
            None => path::PathBuf::from("clang-tidy"),
            // cmd defined as CLI parameter but not in the .json configuration file
            Some(cmd_cli) => path::PathBuf::from(cmd_cli.as_path()),
        },
        Some(cmd_cfg) => match &data.command {
            // cmd defined in the .json configuration file but not as CLI parameter
            None => {
                from_json = true;
                path::PathBuf::from(cmd_cfg.as_path())
            }
            // cmd defined in both, the .json configuration file and as CLI parameter
            Some(cmd_cli) => {
                log::debug!(
                    "Override detected:\nCommand '{}' \
                        specified in '{}' is overridden by the command-line parameter: '{}'\n",
                    cmd_cfg.to_string_lossy(),
                    data.json.name,
                    cmd_cli.as_path().to_string_lossy()
                );
                path::PathBuf::from(cmd_cli.as_path())
            }
        },
    };

    if from_json {
        return utils::executable_or_exists(cmd.as_path(), Some(data.json.root.as_path()))
            .wrap_err("Invalid configuration for field 'command'")
            .suggestion(
                "When using relative paths for the field 'command' please \
                    make sure to provide a valid path relative to the \
                    <JSON> root directory.",
            );
    }
    Ok(cmd)
}

pub fn git_between_paths(data: &cli::Data, range: &str) -> eyre::Result<Vec<path::PathBuf>> {
    let repo_root = git_repo_root(data.json.root.as_path())?;
    let output = git_diff_output(repo_root.as_path(), parse_between_spec(range)?)
        .wrap_err("Failed to execute git diff for option '--between'")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            format!("git diff returned status {}", output.status)
        } else {
            stderr
        };
        return Err(eyre!("Failed to resolve files for git range '{range}': {message}"))
            .suggestion("Please make sure both revisions exist and the current workspace is inside a git repository");
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let path = repo_root.join(line);
            fs::canonicalize(&path).wrap_err(format!(
                "Failed to resolve changed file '{}' from git range '{}'",
                path.to_string_lossy(),
                range
            ))
        })
        .collect()
}

fn git_repo_root(start: &path::Path) -> eyre::Result<path::PathBuf> {
    let output = process::Command::new("git")
        .arg("-C")
        .arg(start)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .wrap_err("Failed to execute git rev-parse")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            format!("git rev-parse returned status {}", output.status)
        } else {
            stderr
        };
        return Err(eyre!(
            "Failed to determine the git repository root from '{}': {message}",
            start.to_string_lossy()
        ))
        .suggestion("Please run this command inside a git workspace when using '--between'");
    }

    Ok(path::PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum BetweenSpec<'a> {
    Range(&'a str),
    Staged(&'a str),
}

fn parse_between_spec(range: &str) -> eyre::Result<BetweenSpec<'_>> {
    if range.contains("...") {
        return Ok(BetweenSpec::Range(range));
    }

    if let Some(base) = range.strip_suffix("..") {
        if base.is_empty() {
            return Err(eyre!("Invalid parameter for option --between: {range}"))
                .suggestion("Please provide a base revision before '..', e.g. 'HEAD..'");
        }
        return Ok(BetweenSpec::Staged(base));
    }

    Err(eyre!("Invalid parameter for option --between: {range}"))
        .suggestion("Please provide '<BASE>...<HEAD>' or '<BASE>..' for staged changes")
}

fn git_diff_output(repo_root: &path::Path, spec: BetweenSpec<'_>) -> io::Result<process::Output> {
    let mut cmd = process::Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--name-only")
        .arg("--diff-filter=ACMR");

    match spec {
        BetweenSpec::Range(range) => {
            cmd.arg(range);
        }
        BetweenSpec::Staged(base) => {
            cmd.arg("--cached").arg(base);
        }
    }

    cmd.output()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{git_repo_root, parse_between_spec, BetweenSpec};

    #[test]
    #[cfg(not(windows))]
    fn test_command_path() {
        #[derive(Debug)]
        struct TestPaths {
            path: PathBuf,
            is_absolute: bool,
            is_relative: bool,
            is_file: bool,
        }

        let tests = vec![
            TestPaths {
                path: "some/path/to/clang-tidy".into(),
                is_absolute: false,
                is_relative: true,
                is_file: false,
            },
            TestPaths {
                path: "/some/path/to/clang-tidy".into(),
                is_absolute: true,
                is_relative: false,
                is_file: false,
            },
            TestPaths {
                path: "clang-tidy".into(),
                is_absolute: false,
                is_relative: true,
                is_file: true,
            },
            TestPaths {
                path: "clang-tidy.exe".into(),
                is_absolute: false,
                is_relative: true,
                is_file: true,
            },
            TestPaths {
                path: "some/path/..".into(),
                is_absolute: false,
                is_relative: true,
                is_file: false,
            },
        ];

        fn test_paths(paths: &[TestPaths]) {
            for path in paths.iter() {
                println!("checking {path:?}");
                println!("  components{}", path.path.components().count());
                assert_eq!(path.is_absolute, path.path.is_absolute());
                assert_eq!(path.is_relative, path.path.is_relative());

                // one way to check that the path passed for "command" is a pure file or
                // executable name is to count the components
                assert_eq!(path.is_file, path.path.components().count() == 1);

                // another way is to take the filename and compare it to the original path. if the
                // (complete) original path is the same, it is just a filename.
                let is_file = path
                    .path
                    .file_name()
                    .and_then(|file_name| (path.path.as_os_str() == file_name).then_some(file_name))
                    .is_some();

                assert_eq!(path.is_file, is_file);
            }
        }

        test_paths(&tests);
    }

    #[test]
    fn git_repo_root_requires_repository() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("run-clang-tidy-no-git-{suffix}"));
        fs::create_dir_all(&temp).unwrap();
        let result = git_repo_root(&temp);
        fs::remove_dir_all(&temp).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn parse_between_accepts_revision_range_and_staged_forms() {
        assert_eq!(
            parse_between_spec("main...HEAD").unwrap(),
            BetweenSpec::Range("main...HEAD")
        );
        assert_eq!(
            parse_between_spec("HEAD..").unwrap(),
            BetweenSpec::Staged("HEAD")
        );
        assert!(parse_between_spec("..").is_err());
        assert!(parse_between_spec("HEAD").is_err());
    }
}
