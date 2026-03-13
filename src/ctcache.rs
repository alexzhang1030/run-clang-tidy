use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    process,
    sync::Arc,
};

use color_eyre::eyre::WrapErr;
use serde::{Deserialize, Serialize};

use crate::cmd;

#[derive(Debug, Clone)]
pub struct Context {
    build_root: PathBuf,
    clang_tidy: PathBuf,
    clang_tidy_version: String,
    cache_dir: PathBuf,
    compile_commands: Arc<HashMap<PathBuf, CompileCommand>>,
}

#[derive(Debug, Deserialize)]
struct CompileCommand {
    directory: Option<PathBuf>,
    file: PathBuf,
    command: Option<String>,
    arguments: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    return_code: i32,
    stdout: String,
    stderr: String,
}

impl Context {
    pub fn new(
        enabled: bool,
        build_root: &Path,
        clang_tidy: &Path,
        clang_tidy_version: Option<&str>,
    ) -> eyre::Result<Option<Self>> {
        if !enabled {
            return Ok(None);
        }

        let compile_commands = load_compile_commands(build_root)?;
        let cache_dir = default_cache_dir();
        fs::create_dir_all(&cache_dir).wrap_err(format!(
            "Failed to create ctcache directory {}",
            cache_dir.to_string_lossy()
        ))?;

        Ok(Some(Self {
            build_root: build_root.to_path_buf(),
            clang_tidy: clang_tidy.to_path_buf(),
            clang_tidy_version: clang_tidy_version.unwrap_or("unknown").to_string(),
            cache_dir,
            compile_commands: Arc::new(compile_commands),
        }))
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn run_or_execute(
        &self,
        runner: &cmd::Runner,
        path: &Path,
        fix: bool,
        ignore_warn: bool,
    ) -> cmd::RunResult {
        if fix {
            return runner.run_tidy(path, &self.build_root, fix, ignore_warn);
        }

        let Some(digest) = self.digest_for(path).ok().flatten() else {
            return runner.run_tidy(path, &self.build_root, fix, ignore_warn);
        };

        if let Ok(Some(entry)) = self.load(&digest) {
            return entry.into_run_result(ignore_warn);
        }

        let result = runner.run_tidy(path, &self.build_root, fix, ignore_warn);
        if let Some(entry) = CacheEntry::from_run_result(&result) {
            let _ = self.store(&digest, &entry);
        }
        result
    }

    fn digest_for(&self, path: &Path) -> eyre::Result<Option<String>> {
        let mut hash = HashBuilder::new();
        hash.update(self.clang_tidy.to_string_lossy().as_bytes());
        hash.update(self.clang_tidy_version.as_bytes());

        let canonical = path.canonicalize().wrap_err(format!(
            "Failed to canonicalize source path {}",
            path.to_string_lossy()
        ))?;
        hash.update(canonical.to_string_lossy().as_bytes());

        let source = fs::read(&canonical).wrap_err(format!(
            "Failed to read source file {}",
            canonical.to_string_lossy()
        ))?;
        hash.update(&source);

        if let Some(command) = self.compile_commands.get(&canonical) {
            if let Some(directory) = &command.directory {
                hash.update(directory.to_string_lossy().as_bytes());
            }
            if let Some(command_line) = &command.command {
                hash.update(command_line.as_bytes());
            }
            if let Some(arguments) = &command.arguments {
                for argument in arguments {
                    hash.update(argument.as_bytes());
                    hash.update(&[0]);
                }
            }
        }

        let dump_config = self.dump_config(&canonical)?;
        hash.update(&dump_config);

        Ok(Some(hash.finish_hex()))
    }

    fn dump_config(&self, path: &Path) -> eyre::Result<Vec<u8>> {
        let output = process::Command::new(&self.clang_tidy)
            .arg("--dump-config")
            .arg(path)
            .arg(format!("-p={}", self.build_root.to_string_lossy()))
            .output()
            .wrap_err("Failed to execute clang-tidy --dump-config for ctcache")?;

        if !output.status.success() || output.stdout.is_empty() {
            return Err(eyre::eyre!(format!(
                "clang-tidy --dump-config failed for {}",
                path.to_string_lossy()
            )));
        }

        Ok(output.stdout)
    }

    fn store(&self, digest: &str, entry: &CacheEntry) -> eyre::Result<()> {
        let path = self.cache_file_path(digest);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).wrap_err(format!(
                "Failed to create ctcache subdirectory {}",
                parent.to_string_lossy()
            ))?;
        }
        let tmp = path.with_extension("tmp");
        let file = fs::File::create(&tmp).wrap_err(format!(
            "Failed to create temporary ctcache file {}",
            tmp.to_string_lossy()
        ))?;
        serde_json::to_writer(file, entry).map_err(io::Error::other)?;
        fs::rename(&tmp, &path).wrap_err(format!(
            "Failed to move ctcache file into place {}",
            path.to_string_lossy()
        ))?;
        Ok(())
    }

    fn load(&self, digest: &str) -> eyre::Result<Option<CacheEntry>> {
        let path = self.cache_file_path(digest);
        if !path.exists() {
            return Ok(None);
        }

        let file = fs::File::open(&path).wrap_err(format!(
            "Failed to open ctcache file {}",
            path.to_string_lossy()
        ))?;
        let entry = serde_json::from_reader(file).wrap_err(format!(
            "Failed to parse ctcache file {}",
            path.to_string_lossy()
        ))?;
        Ok(Some(entry))
    }

    fn cache_file_path(&self, digest: &str) -> PathBuf {
        let (prefix, suffix) = digest.split_at(2.min(digest.len()));
        self.cache_dir.join(prefix).join(format!("{suffix}.json"))
    }
}

impl CacheEntry {
    fn from_run_result(result: &cmd::RunResult) -> Option<Self> {
        match result {
            cmd::RunResult::Ok => Some(Self {
                return_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }),
            cmd::RunResult::Warn(details) => Some(Self {
                return_code: 0,
                stdout: details.stdout.clone(),
                stderr: details.stderr.clone(),
            }),
            cmd::RunResult::Err(_) => None,
        }
    }

    fn into_run_result(self, ignore_warn: bool) -> cmd::RunResult {
        let details = cmd::RunDetails {
            process_error: None,
            stdout: self.stdout,
            stderr: self.stderr,
        };

        if self.return_code != 0 {
            let mut details = details;
            details.process_error =
                Some(format!("Process terminated with code {}", self.return_code));
            return cmd::RunResult::Err(details);
        }

        if !ignore_warn && !details.stderr.is_empty() {
            cmd::RunResult::Warn(details)
        } else {
            cmd::RunResult::Ok
        }
    }
}

fn load_compile_commands(build_root: &Path) -> eyre::Result<HashMap<PathBuf, CompileCommand>> {
    let path = build_root.join("compile_commands.json");
    let file = fs::File::open(&path).wrap_err(format!(
        "Failed to open compilation database {}",
        path.to_string_lossy()
    ))?;
    let commands: Vec<CompileCommand> = serde_json::from_reader(file).wrap_err(format!(
        "Failed to parse compilation database {}",
        path.to_string_lossy()
    ))?;

    let mut map = HashMap::with_capacity(commands.len());
    for command in commands {
        let file = if command.file.is_absolute() {
            command.file.clone()
        } else if let Some(directory) = &command.directory {
            directory.join(&command.file)
        } else {
            command.file.clone()
        };

        if let Ok(file) = file.canonicalize() {
            map.insert(file, command);
        }
    }
    Ok(map)
}

fn default_cache_dir() -> PathBuf {
    if let Ok(path) = std::env::var("CTCACHE_DIR") {
        return PathBuf::from(path);
    }

    std::env::temp_dir().join("ct-cache")
}

struct HashBuilder {
    hi: u64,
    lo: u64,
}

impl HashBuilder {
    fn new() -> Self {
        Self {
            hi: 0xcbf29ce484222325,
            lo: 0x84222325cbf29ce4,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.hi ^= u64::from(*byte);
            self.hi = self.hi.wrapping_mul(0x100000001b3);
            self.lo ^= u64::from(*byte).rotate_left(1);
            self.lo = self.lo.wrapping_mul(0x100000001b3);
        }
        self.hi ^= bytes.len() as u64;
        self.lo ^= (bytes.len() as u64).rotate_left(7);
    }

    fn finish_hex(&self) -> String {
        format!("{:016x}{:016x}", self.hi, self.lo)
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheEntry, HashBuilder};
    use crate::cmd::RunResult;

    #[test]
    fn hash_builder_is_stable() {
        let mut hash = HashBuilder::new();
        hash.update(b"abc");
        hash.update(b"def");
        assert_eq!(hash.finish_hex(), "be16713faf727a4cd41df0ddea85ac7a");
    }

    #[test]
    fn cached_warning_roundtrip_preserves_output() {
        let entry = CacheEntry {
            return_code: 0,
            stdout: String::new(),
            stderr: "warning text".to_string(),
        };

        match entry.into_run_result(false) {
            RunResult::Warn(details) => assert_eq!(details.stderr, "warning text"),
            other => panic!("unexpected cached result: {other:?}"),
        }
    }
}
