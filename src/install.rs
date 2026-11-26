//! 照登记表的 `distribution` 把一个 agent 装好、给出怎么起它（设计 73 第四节第 2 条）。
//!
//! - 先看 `binary`：这个系统、这种架构有（[`platform`]，Linux 上没有 glibc 版的退到 `-musl` 版），而且写了 `sha256` 的，下载进
//!   状态目录的 `agents/<编号>/<版本>/`，核对 SHA-256，解开（[`crate::archive`]），照 `cmd`、`args`、`env` 起。核不上的拒装。
//!   装好写一个 `.installed` 记号；换了版本，同一个 agent 的旧版本目录删掉。没写 `sha256` 的一律不装（核不了就不装）。
//! - 再看 `npx`、`uvx`：机器上有这个程序就照包名、版本起，包缓存放在缓存目录下（`npm_config_cache`、`UV_CACHE_DIR`）；没有的报
//!   「要先装 Node.js（uv）」。
//! - 都没有的：这个系统装不上（[`plan`] 交 `None`，`driver.models` 不列它）。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::archive::{self, Kind};

/// 压缩包至多多大（2026-11-26 最大的是 Antigravity，274 MB）。
const DOWNLOAD_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// 装好以后怎么起一个 agent。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// 程序。
    pub program: PathBuf,
    /// 参数。
    pub args: Vec<String>,
    /// 另加的环境变量。
    pub env: Vec<(String, String)>,
}

/// 照 `distribution` 定的装法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// 下载、核对、解开。
    Binary {
        /// 压缩包（或裸程序）的网址。
        archive: String,
        /// 登记表写的 SHA-256。
        sha256: String,
        /// 解开以后的程序（相对目录，`./claude-agent-acp` 这样）。
        cmd: String,
        /// 参数。
        args: Vec<String>,
        /// 环境变量。
        env: Vec<(String, String)>,
    },
    /// `npx -y <包>`。
    Npx {
        /// 包名和版本（`@agentclientprotocol/claude-agent-acp@0.112.0`）。
        package: String,
        /// 参数。
        args: Vec<String>,
        /// 环境变量。
        env: Vec<(String, String)>,
    },
    /// `uvx`。
    Uvx {
        /// 包名和版本（`fast-agent-acp==0.10.66`）。
        package: String,
        /// 参数。
        args: Vec<String>,
        /// 环境变量。
        env: Vec<(String, String)>,
    },
}

/// 这台机器在登记表里的写法：`linux-x86_64`、`darwin-aarch64` 这样。
#[must_use]
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// 照 `distribution` 定这台机器（`platform`）的装法：`binary`（有 `sha256` 的）优先，再 `npx`，再 `uvx`；都没有是 `None`。
#[must_use]
pub fn plan(distribution: &Value, platform: &str) -> Option<Plan> {
    let binary = distribution.get("binary").and_then(|binary| {
        let musl = format!("{platform}-musl");
        let musl_first = cfg!(target_env = "musl");
        let order = if musl_first {
            [musl.as_str(), platform]
        } else {
            [platform, musl.as_str()]
        };
        order.iter().find_map(|key| {
            let target = binary.get(*key)?;
            Some(Plan::Binary {
                archive: target.get("archive")?.as_str()?.to_string(),
                sha256: target
                    .get("sha256")?
                    .as_str()
                    .filter(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))?
                    .to_ascii_lowercase(),
                cmd: target.get("cmd")?.as_str()?.to_string(),
                args: strings(target.get("args")),
                env: pairs(target.get("env")),
            })
        })
    });
    binary.or_else(|| {
        let package = |kind: &str| {
            let spec = distribution.get(kind)?;
            Some((
                spec.get("package")?.as_str()?.to_string(),
                strings(spec.get("args")),
                pairs(spec.get("env")),
            ))
        };
        if let Some((package, args, env)) = package("npx") {
            Some(Plan::Npx { package, args, env })
        } else {
            package("uvx").map(|(package, args, env)| Plan::Uvx { package, args, env })
        }
    })
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn pairs(value: Option<&Value>) -> Vec<(String, String)> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// 照 `plan` 装好编号 `id`、版本 `version` 的 agent，交怎么起它。装的东西放 `state`（`agents/` 下），包缓存放 `cache`。
///
/// # Errors
///
/// 下载、核对、解开不成，没有 `npx`、`uvx`：给人看的英文一句。
pub fn ensure(
    plan: &Plan,
    id: &str,
    version: &str,
    state: &Path,
    cache: &Path,
) -> Result<Launch, String> {
    match plan {
        Plan::Binary {
            archive,
            sha256,
            cmd,
            args,
            env,
        } => {
            let dir = binary(
                archive,
                sha256,
                cmd,
                &state.join("agents").join(id),
                version,
            )?;
            Ok(Launch {
                program: dir.join(cmd),
                args: args.clone(),
                env: env.clone(),
            })
        }
        Plan::Npx { package, args, env } => {
            let npx = find_program("npx").ok_or_else(|| {
                format!("{id} runs through npx, which was not found on PATH; install Node.js first")
            })?;
            let mut all = vec!["-y".to_string(), package.clone()];
            all.extend(args.iter().cloned());
            let mut env = env.clone();
            env.push((
                "npm_config_cache".into(),
                cache.join("npm").display().to_string(),
            ));
            Ok(Launch {
                program: npx,
                args: all,
                env,
            })
        }
        Plan::Uvx { package, args, env } => {
            let uvx = find_program("uvx").ok_or_else(|| {
                format!("{id} runs through uvx, which was not found on PATH; install uv first")
            })?;
            // `pkg==1.0` 这种写法要用 `--from` 交（uv 的说明），程序名是包名。
            let mut all = match package.split_once("==") {
                Some((name, _)) => vec!["--from".to_string(), package.clone(), name.to_string()],
                None => vec![package.clone()],
            };
            all.extend(args.iter().cloned());
            let mut env = env.clone();
            env.push((
                "UV_CACHE_DIR".into(),
                cache.join("uv").display().to_string(),
            ));
            Ok(Launch {
                program: uvx,
                args: all,
                env,
            })
        }
    }
}

/// 装 `binary` 那一种：交装好的目录（`<root>/<版本>`）。
fn binary(
    url: &str,
    sha256: &str,
    cmd: &str,
    root: &Path,
    version: &str,
) -> Result<PathBuf, String> {
    let version = if version.is_empty() {
        "current"
    } else {
        version
    };
    if version.contains(['/', '\\']) || version.starts_with('.') {
        return Err(format!("the registry gives an odd version {version:?}"));
    }
    let dir = root.join(version);
    if dir.join(".installed").is_file() {
        return Ok(dir);
    }
    let failed =
        |error: std::io::Error| format!("cannot install under {}: {error}", root.display());
    std::fs::create_dir_all(root).map_err(failed)?;
    let partial = root.join(format!(".partial-{}", crate::workspace::unique()));
    std::fs::create_dir(&partial).map_err(failed)?;
    let result = (|| {
        let download = partial.join(".download");
        eprintln!("installing: downloading {url}");
        let got = crate::fetch::download(url, &download, DOWNLOAD_LIMIT)?;
        if got != sha256 {
            return Err(format!(
                "the download from {url} does not match the registry's SHA-256 (expected {sha256}, got {got}); not installed"
            ));
        }
        archive::extract(&download, Kind::of(url), &partial, cmd)?;
        std::fs::remove_file(&download).map_err(failed)?;
        let program = partial.join(cmd);
        if !program.is_file() {
            return Err(format!("the archive from {url} has no {cmd}"));
        }
        archive::make_executable(&program).map_err(failed)?;
        std::fs::write(partial.join(".installed"), sha256).map_err(failed)?;
        crate::workspace::remove(&dir).map_err(failed)?;
        std::fs::rename(&partial, &dir).map_err(failed)
    })();
    if let Err(why) = result {
        drop(crate::workspace::remove(&partial));
        return Err(why);
    }
    eprintln!("installing: {} is ready", dir.display());
    remove_other_versions(root, version);
    Ok(dir)
}

/// 删掉 `root` 下除了 `keep` 以外的版本目录（换版本时旧的删掉）。正在装的（`.partial-*`）不动。
fn remove_other_versions(root: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != keep
            && !name.starts_with('.')
            && let Err(error) = crate::workspace::remove(&entry.path())
        {
            eprintln!(
                "could not remove the old version {}: {error}",
                entry.path().display()
            );
        }
    }
}

/// 在 `PATH` 里找可执行的 `name`。
#[must_use]
pub fn find_program(name: &str) -> Option<PathBuf> {
    find_program_in(name, &std::env::var_os("PATH")?)
}

/// 在 `path`（`PATH` 的写法）里找可执行的 `name`。
#[must_use]
pub fn find_program_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

#[cfg(test)]
mod tests;
