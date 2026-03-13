use std::{io, path, process, str::FromStr};

#[derive(Clone)]
struct Version {
    major: u8,
    minor: u8,
    patch: u8,
}

#[derive(Debug)]
pub struct RunDetails {
    pub process_error: Option<String>,
    pub stderr: String,
    pub stdout: String,
}

impl RunDetails {
    pub fn from_error(error: impl Into<String>) -> Self {
        Self {
            process_error: Some(error.into()),
            stderr: String::new(),
            stdout: String::new(),
        }
    }

    pub fn diagnostic_text(&self) -> String {
        let mut parts = Vec::new();
        if !self.stderr.trim().is_empty() {
            parts.push(self.stderr.trim_end().to_string());
        }
        if !self.stdout.trim().is_empty() {
            parts.push(self.stdout.trim_end().to_string());
        }
        parts.join("\n")
    }

    pub fn error_message(&self) -> String {
        if let Some(process_error) = &self.process_error {
            let details = self.diagnostic_text();
            if details.is_empty() {
                process_error.clone()
            } else {
                format!("{process_error}\n---\n{details}")
            }
        } else {
            self.diagnostic_text()
        }
    }

    pub fn warning_message(&self) -> String {
        let details = self.diagnostic_text();
        if details.is_empty() {
            "warnings encountered".to_string()
        } else {
            format!("warnings encountered\n---\n{details}")
        }
    }
}

#[derive(Debug)]
pub enum RunResult {
    Ok,
    Err(RunDetails),
    Warn(RunDetails),
}

impl From<&io::Error> for RunResult {
    fn from(value: &io::Error) -> Self {
        RunResult::Err(RunDetails::from_error(value.to_string()))
    }
}

impl FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let re = regex::Regex::new(r".*version ([\d]+)\.([\d]+)\.([\d]+).*").unwrap();
        let caps = re.captures(s).ok_or("Failed to match version")?;

        Ok(Version {
            major: caps[1].parse().map_err(|_| "Invalid major version")?,
            minor: caps[2].parse().map_err(|_| "Invalid minor version")?,
            patch: caps[3].parse().map_err(|_| "Invalid patch level")?,
        })
    }
}

pub struct Runner {
    cmd: path::PathBuf,
    version: Option<Version>,
}

impl Runner {
    pub fn new<P>(path: P) -> Runner
    where
        P: AsRef<path::Path>,
    {
        let cmd = path::PathBuf::from(path.as_ref());
        Runner { cmd, version: None }
    }

    fn eval_status(status: process::ExitStatus) -> Result<(), io::Error> {
        match status.code() {
            Some(0) => (),
            Some(code) => {
                return Err(io::Error::other(format!(
                    "Process terminated with code {code}"
                )));
            }
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Process terminated by signal",
                ))
            }
        };
        Ok(())
    }

    pub fn get_version(&self) -> Option<String> {
        self.version
            .as_ref()
            .map(|v| format!("{}.{}.{}", v.major, v.minor, v.patch))
    }

    pub fn get_path(&self) -> path::PathBuf {
        self.cmd.clone()
    }

    pub fn validate(&mut self) -> Result<(), io::Error> {
        let cmd = process::Command::new(self.cmd.as_path())
            .arg("--version")
            .output()?;

        if let Err(err) = Runner::eval_status(cmd.status) {
            log::error!(
                "Execution failed:\n{}",
                String::from_utf8_lossy(&cmd.stderr)
            );
            return Err(err);
        }

        // example output of clang-format:
        // clang-format version 4.0.0 (tags/checker/checker-279)
        let stdout = String::from_utf8_lossy(&cmd.stdout);

        self.version = Some(stdout.parse::<Version>().map_err(|err| {
            io::Error::other(format!("Failed to parse --version output {stdout}: {err}"))
        })?);
        Ok(())
    }

    fn run(mut cmd: process::Command, ignore_warn: bool) -> RunResult {
        let output = cmd.output();
        if let Err(err) = &output {
            return err.into();
        }
        let output = output.unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let details = RunDetails {
            process_error: None,
            stderr: stderr.into_owned(),
            stdout: stdout.into_owned(),
        };

        if let Err(err) = Runner::eval_status(output.status) {
            let mut details = details;
            details.process_error = Some(err.to_string());
            return RunResult::Err(details);
        } else if !ignore_warn && !details.stderr.is_empty() {
            return RunResult::Warn(details);
        }
        RunResult::Ok
    }

    pub fn run_tidy<P, Q>(&self, file: P, build_root: Q, fix: bool, ignore_warn: bool) -> RunResult
    where
        P: AsRef<path::Path>,
        Q: AsRef<path::Path>,
    {
        let mut cmd = process::Command::new(self.cmd.as_path());

        cmd.arg(file.as_ref().as_os_str());
        // TODO: the --config-file option does not exist for clang-tidy 10.0
        // if let Some(config_file) = config_file {
        //     cmd.arg(format!(
        //         "--config-file={}",
        //         config_file.as_ref().to_string_lossy()
        //     ));
        // }
        cmd.arg(format!("-p={}", build_root.as_ref().to_string_lossy()));
        if fix {
            cmd.arg("-fix").arg("-fix-errors");
        }
        // This suppresses printing statistics about ignored warnings:
        // cmd.arg("-quiet");

        Runner::run(cmd, ignore_warn)
    }

    pub fn supports_config_file(&self) -> Result<(), io::Error> {
        if self.version.is_none() {
            return Err(io::Error::other(
                "Unknown version, --config-file requires \
                clang-format version 12.0.0 or higher",
            ));
        }

        let version = self.version.as_ref().unwrap();
        if version.major < 9u8 {
            return Err(io::Error::other(format!(
                "Invalid version {}, --config-file check requires \
                    clang-format version 12.0.0 or higher",
                self.get_version().unwrap()
            )));
        }

        Ok(())
    }
}

impl Clone for Runner {
    fn clone(&self) -> Runner {
        Runner {
            cmd: path::PathBuf::from(self.cmd.as_path()),
            version: self.version.clone(),
        }
    }
}
