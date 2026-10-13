//! bash 子プロセスから EgoPulse の私有データと秘密を隠す隔離。
//!
//! Linux では bubblewrap (`bwrap`) でホストのファイルシステムをそのまま見せ、
//! [`ProtectedPaths`] だけを方針メッセージに差し替える。あわせて Capability を
//! 絞り、隠したマウントの解除や `/proc/<pid>` 経由の読み取りを防ぐ。PID・
//! ネットワーク・`/run` は分離しないため、`systemctl` や tmux はそのまま使える。
//!
//! 守る対象は、エージェントがふつうの手段で私有データに触れることである。
//! `systemd-run` や `docker` のように隔離の外でプロセスを起動する意図的な迂回は
//! 防がない。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{info, warn};

/// 隠したパスの代わりに見せるメッセージ。
const HIDDEN_MESSAGE: &str = "This path is hidden from bash by EgoPulse's security policy. \
Do not work around it; ask the user if access is required.\n";

/// メッセージを置くファイル名（`runtime_dir` 直下。sandbox の中からは見えない）。
const MESSAGE_FILE_NAME: &str = "bash-sandbox-hidden.txt";

/// 隠したディレクトリの中に置く、メッセージのファイル名。
const HIDDEN_DIR_MARKER: &str = "HIDDEN_BY_POLICY.txt";

/// bwrap を探す場所。`PATH` はエージェントが書き込めるディレクトリを含み得るため、
/// 偽の `bwrap` を掴まないようシステムのディレクトリだけを見る。
const BWRAP_SEARCH_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin", "/bin"];

/// bash に残す Capability。Docker が root のコンテナに既定で与える組から
/// `CAP_MKNOD` を除いたもので、パッケージ管理などの通常の root 作業が動く。
/// 隔離を破る操作に要る `CAP_SYS_ADMIN`（マウント）、`CAP_SYS_PTRACE`（他プロセスの
/// 検査）、`CAP_DAC_READ_SEARCH`（ファイルハンドル経由の読み取り）は含めない。
const RETAINED_CAPABILITIES: &[&str] = &[
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_FSETID",
    "CAP_FOWNER",
    "CAP_NET_RAW",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETFCAP",
    "CAP_SETPCAP",
    "CAP_NET_BIND_SERVICE",
    "CAP_SYS_CHROOT",
    "CAP_KILL",
    "CAP_AUDIT_WRITE",
];

/// bash から隠すパス。
#[derive(Clone, Debug)]
pub(crate) struct ProtectedPaths {
    /// Runtime の私有ディレクトリ。DB・バックアップ・会話アーカイブ・Local API の
    /// ソケットを含み、後から作られるファイルもまとめて隠す。
    pub(crate) runtime_dir: PathBuf,
    /// 秘密の実値を持つ `.env`。存在しない構成もある。
    pub(crate) secrets_file: Option<PathBuf>,
}

/// bash 子プロセスの実行方法。
#[derive(Debug)]
pub(crate) enum BashSandbox {
    /// bubblewrap で隔離する。
    Isolated(BwrapSandbox),
    /// 隔離せずに実行する。理由は状態として公開する。
    Unisolated { reason: String },
}

/// 隔離の状態。`/api/status` で公開する。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BashSandboxStatus {
    pub(crate) enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Debug)]
pub(crate) struct BwrapSandbox {
    bwrap: PathBuf,
    message_file: PathBuf,
    protected: ProtectedPaths,
}

impl BashSandbox {
    /// 隔離せずに実行する。
    pub(crate) fn unisolated(reason: impl Into<String>) -> Self {
        Self::Unisolated {
            reason: reason.into(),
        }
    }

    /// 使える場合は bubblewrap で隔離する。使えない場合は隔離せず、理由を警告として
    /// 記録する。
    pub(crate) fn detect(protected: ProtectedPaths) -> Self {
        let sandbox = Self::detect_in(BWRAP_SEARCH_DIRS, protected);
        match &sandbox {
            Self::Isolated(isolated) => {
                info!(bwrap = %isolated.bwrap.display(), "bash runs in a bubblewrap sandbox");
            }
            Self::Unisolated { reason } => {
                warn!(%reason, "bash runs without isolation");
            }
        }
        sandbox
    }

    fn detect_in(search_dirs: &[&str], protected: ProtectedPaths) -> Self {
        if !cfg!(target_os = "linux") {
            return Self::unisolated("the bash sandbox requires Linux");
        }
        let Some(bwrap) = find_bwrap(search_dirs) else {
            return Self::unisolated(format!(
                "bubblewrap (bwrap) was not found in {}",
                search_dirs.join(", ")
            ));
        };
        if let Err(reason) = probe(&bwrap) {
            return Self::unisolated(reason);
        }
        let message_file = protected.runtime_dir.join(MESSAGE_FILE_NAME);
        if let Err(error) = std::fs::create_dir_all(&protected.runtime_dir)
            .and_then(|()| std::fs::write(&message_file, HIDDEN_MESSAGE))
        {
            return Self::unisolated(format!(
                "failed to write {}: {error}",
                message_file.display()
            ));
        }
        Self::Isolated(BwrapSandbox {
            bwrap,
            message_file,
            protected,
        })
    }

    pub(crate) fn status(&self) -> BashSandboxStatus {
        match self {
            Self::Isolated(_) => BashSandboxStatus {
                enabled: true,
                reason: None,
            },
            Self::Unisolated { reason } => BashSandboxStatus {
                enabled: false,
                reason: Some(reason.clone()),
            },
        }
    }

    /// `bash --noprofile --norc -c <command>` を実行するコマンドを組み立てる。
    pub(crate) fn bash_command(&self, command: &str) -> Command {
        let mut cmd = match self {
            Self::Isolated(sandbox) => {
                let mut cmd = Command::new(&sandbox.bwrap);
                cmd.args(sandbox.args());
                cmd.arg("bash");
                cmd
            }
            Self::Unisolated { .. } => Command::new("bash"),
        };
        cmd.args(["--noprofile", "--norc", "-c", command]);
        cmd
    }
}

impl BwrapSandbox {
    /// 起動のたびに組み立てる。存在しないパスは隠さない（bind するとホスト側に
    /// マウントポイントが作られてしまうため）。
    fn args(&self) -> Vec<OsString> {
        let mut args = base_args();
        if let Some(secrets_file) = self
            .protected
            .secrets_file
            .as_deref()
            .filter(|path| path.is_file())
        {
            args.extend(ro_bind(&self.message_file, secrets_file));
        }
        let runtime_dir = &self.protected.runtime_dir;
        if runtime_dir.is_dir() {
            args.extend(["--tmpfs".into(), runtime_dir.into()]);
            args.extend(ro_bind(
                &self.message_file,
                &runtime_dir.join(HIDDEN_DIR_MARKER),
            ));
            args.extend(["--remount-ro".into(), runtime_dir.into()]);
        }
        args.push("--".into());
        args
    }
}

fn ro_bind(source: &Path, dest: &Path) -> [OsString; 3] {
    ["--ro-bind".into(), source.into(), dest.into()]
}

/// ホストのルートをそのまま見せ、Capability を [`RETAINED_CAPABILITIES`] に絞る。
fn base_args() -> Vec<OsString> {
    let mut args: Vec<OsString> = ["--bind", "/", "/", "--dev-bind", "/dev", "/dev"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.extend(["--cap-drop".into(), "ALL".into()]);
    for capability in RETAINED_CAPABILITIES {
        args.extend(["--cap-add".into(), (*capability).into()]);
    }
    args
}

fn find_bwrap(search_dirs: &[&str]) -> Option<PathBuf> {
    search_dirs
        .iter()
        .map(|dir| Path::new(dir).join("bwrap"))
        .find(|path| path.is_file())
}

/// 実際に起動できるかを確かめる。インストール済みでも、ユーザー名前空間が
/// 制限された環境では起動できないことがある。
fn probe(bwrap: &Path) -> Result<(), String> {
    let mut args = base_args();
    args.extend(["--".into(), "true".into()]);
    let output = std::process::Command::new(bwrap)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to run {}: {error}", bwrap.display()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{} cannot start a sandbox: {}",
        bwrap.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        protected: ProtectedPaths,
    }

    /// `root/runtime`（DB 入り）と `root/.env` を持つ state root を作る。
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let runtime_dir = root.join("runtime");
        std::fs::create_dir_all(runtime_dir.join("backups")).expect("runtime dir");
        std::fs::write(runtime_dir.join("egopulse.db"), "SQLite format 3").expect("db");
        std::fs::write(root.join(".env"), "TOKEN=plaintext-secret\n").expect("dotenv");
        let protected = ProtectedPaths {
            runtime_dir,
            secrets_file: Some(root.join(".env")),
        };
        Fixture {
            _dir: dir,
            root,
            protected,
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn sandbox_for(protected: ProtectedPaths) -> BwrapSandbox {
        BwrapSandbox {
            bwrap: PathBuf::from("/usr/bin/bwrap"),
            message_file: protected.runtime_dir.join(MESSAGE_FILE_NAME),
            protected,
        }
    }

    #[test]
    fn args_hide_the_secrets_file_and_the_runtime_dir() {
        // Arrange
        let fixture = fixture();
        let sandbox = sandbox_for(fixture.protected.clone());
        let root = fixture.root.display();
        let message = format!("{root}/runtime/{MESSAGE_FILE_NAME}");

        // Act
        let args = strings(&sandbox.args());

        // Assert
        let hiding_start = args
            .iter()
            .position(|arg| arg == "--ro-bind")
            .expect("hiding args");
        assert_eq!(
            args[hiding_start..],
            [
                "--ro-bind".to_string(),
                message.clone(),
                format!("{root}/.env"),
                "--tmpfs".to_string(),
                format!("{root}/runtime"),
                "--ro-bind".to_string(),
                message,
                format!("{root}/runtime/{HIDDEN_DIR_MARKER}"),
                "--remount-ro".to_string(),
                format!("{root}/runtime"),
                "--".to_string(),
            ]
        );
    }

    #[test]
    fn args_keep_only_the_retained_capabilities() {
        let fixture = fixture();
        let sandbox = sandbox_for(fixture.protected.clone());

        let args = strings(&sandbox.args());

        let drop_all = args
            .windows(2)
            .position(|pair| pair == ["--cap-drop", "ALL"])
            .expect("drops every capability first");
        let added: Vec<&str> = args
            .windows(2)
            .filter(|pair| pair[0] == "--cap-add")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(added, RETAINED_CAPABILITIES);
        let first_add = args
            .iter()
            .position(|arg| arg == "--cap-add")
            .expect("adds");
        assert!(drop_all < first_add);
        for forbidden in ["CAP_SYS_ADMIN", "CAP_SYS_PTRACE", "CAP_DAC_READ_SEARCH"] {
            assert!(
                !added.contains(&forbidden),
                "{forbidden} must not be retained"
            );
        }
    }

    #[test]
    fn args_skip_paths_that_do_not_exist() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let sandbox = sandbox_for(ProtectedPaths {
            runtime_dir: dir.path().join("missing-runtime"),
            secrets_file: Some(dir.path().join(".env")),
        });

        // Act
        let args = strings(&sandbox.args());

        // Assert
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--ro-bind" || arg == "--tmpfs")
        );
        assert_eq!(args.last().map(String::as_str), Some("--"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn missing_bwrap_falls_back_to_unisolated_with_a_reason() {
        let fixture = fixture();
        let empty = tempfile::tempdir().expect("tempdir");
        let search_dir = empty.path().to_str().expect("utf8");

        let sandbox = BashSandbox::detect_in(&[search_dir], fixture.protected.clone());

        let status = sandbox.status();
        assert!(!status.enabled);
        assert!(
            status
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("bubblewrap")),
            "{status:?}"
        );
        assert_eq!(sandbox.bash_command("true").as_std().get_program(), "bash");
    }

    #[test]
    fn status_serializes_the_reason_only_when_disabled() {
        let disabled = BashSandbox::unisolated("no bwrap").status();
        let enabled = BashSandboxStatus {
            enabled: true,
            reason: None,
        };

        assert_eq!(
            serde_json::to_value(&disabled).expect("json"),
            serde_json::json!({"enabled": false, "reason": "no bwrap"})
        );
        assert_eq!(
            serde_json::to_value(&enabled).expect("json"),
            serde_json::json!({"enabled": true})
        );
    }

    /// bwrap が実際に使える環境でだけ、隔離を作る。
    fn real_sandbox(protected: ProtectedPaths) -> Option<BashSandbox> {
        let sandbox = BashSandbox::detect_in(BWRAP_SEARCH_DIRS, protected);
        matches!(sandbox, BashSandbox::Isolated(_)).then_some(sandbox)
    }

    async fn run(sandbox: &BashSandbox, script: &str) -> String {
        let output = sandbox
            .bash_command(script)
            .output()
            .await
            .expect("run bash");
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn running_as_root() -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    #[tokio::test]
    async fn protected_paths_show_only_the_policy_message() {
        // Arrange
        let fixture = fixture();
        let Some(sandbox) = real_sandbox(fixture.protected.clone()) else {
            eprintln!("skipped: bwrap is not usable here");
            return;
        };
        let root = fixture.root.display();

        // Act
        let secrets = run(&sandbox, &format!("cat {root}/.env")).await;
        let listing = run(&sandbox, &format!("ls -A {root}/runtime")).await;
        let db = run(&sandbox, &format!("cat {root}/runtime/egopulse.db 2>&1")).await;
        let write = run(&sandbox, &format!("touch {root}/runtime/x 2>&1; echo $?")).await;

        // Assert
        assert_eq!(secrets, HIDDEN_MESSAGE);
        assert_eq!(listing.trim(), HIDDEN_DIR_MARKER);
        assert!(!db.contains("SQLite"), "{db}");
        assert_ne!(write.trim().lines().last(), Some("0"), "{write}");
        assert_eq!(
            std::fs::read_to_string(fixture.root.join(".env")).expect("host dotenv"),
            "TOKEN=plaintext-secret\n"
        );
    }

    #[tokio::test]
    async fn protected_paths_cannot_be_reached_around_the_sandbox() {
        // Arrange: a host process keeps the DB open, as the Runtime does.
        let fixture = fixture();
        let Some(sandbox) = real_sandbox(fixture.protected.clone()) else {
            eprintln!("skipped: bwrap is not usable here");
            return;
        };
        let db = fixture.protected.runtime_dir.join("egopulse.db");
        let mut holder = std::process::Command::new("sh")
            .arg("-c")
            .arg("exec 9<\"$1\"; exec sleep 30")
            .arg("sh")
            .arg(&db)
            .spawn()
            .expect("spawn holder");
        let pid = holder.id();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let root = fixture.root.display();

        // Act
        let output = run(
            &sandbox,
            &format!(
                "umount {root}/runtime 2>&1; cat {root}/runtime/egopulse.db 2>&1; \
                 cat /proc/{pid}/root{root}/runtime/egopulse.db 2>&1; \
                 cat /proc/{pid}/fd/9 2>&1"
            ),
        )
        .await;
        // Control: without the sandbox the open descriptor exposes the DB.
        let control = run(
            &BashSandbox::unisolated("control"),
            &format!("cat /proc/{pid}/fd/9 2>&1"),
        )
        .await;
        let _ = holder.kill();
        let _ = holder.wait();

        // Assert
        assert!(control.contains("SQLite"), "{control}");
        assert!(!output.contains("SQLite"), "{output}");
    }

    #[tokio::test]
    async fn other_processes_environment_is_unreadable() {
        // Arrange: a same-user process stays readable unless the caller has
        // fewer capabilities than it, which only holds for root.
        let fixture = fixture();
        if !running_as_root() {
            eprintln!("skipped: needs root");
            return;
        }
        let Some(sandbox) = real_sandbox(fixture.protected.clone()) else {
            eprintln!("skipped: bwrap is not usable here");
            return;
        };
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .env("SANDBOX_PROBE_KEY", "probe-value")
            .spawn()
            .expect("spawn sleep");
        let pid = other.id();

        // Act
        let output = run(&sandbox, &format!("cat /proc/{pid}/environ 2>&1")).await;
        let control = run(
            &BashSandbox::unisolated("control"),
            &format!("cat /proc/{pid}/environ 2>&1"),
        )
        .await;
        let _ = other.kill();
        let _ = other.wait();

        // Assert
        assert!(control.contains("probe-value"), "{control}");
        assert!(!output.contains("probe-value"), "{output}");
    }

    #[tokio::test]
    async fn root_work_such_as_package_management_still_works() {
        // Arrange
        let fixture = fixture();
        if !running_as_root() {
            eprintln!("skipped: needs root");
            return;
        }
        let Some(sandbox) = real_sandbox(fixture.protected.clone()) else {
            eprintln!("skipped: bwrap is not usable here");
            return;
        };
        let file = fixture.root.join("owned-by-nobody");
        std::fs::write(&file, "").expect("file");

        // Act: change ownership, and drop privileges the way apt does.
        let output = run(
            &sandbox,
            &format!(
                "chown nobody {} && echo chown-ok; \
                 setpriv --reuid=nobody --regid=nogroup --clear-groups id -un",
                file.display()
            ),
        )
        .await;

        // Assert
        assert!(output.contains("chown-ok"), "{output}");
        assert!(output.contains("nobody"), "{output}");
    }
}
