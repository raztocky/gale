//! Locking down the renderer process (Linux).
//!
//! The renderer parses and lays out untrusted web content, so it's the
//! process most likely to be compromised by a bug. Once sandboxed it can only
//! talk to the browser over its stdin/stdout pipes:
//!
//! - **rlimits:** bounded memory and CPU time, no core dumps.
//! - **Landlock:** no filesystem access at all, no TCP bind/connect, no
//!   signals or abstract UNIX sockets outside the sandbox.
//! - **seccomp-bpf:** a syscall allowlist. Everything else (open, socket,
//!   connect, execve, ptrace, ...) fails with EPERM.
//!
//! Fonts are compiled into the binary, so the renderer never needs to open a
//! file after it starts.

#[derive(Debug)]
pub struct Report {
    pub landlock: &'static str,
    pub seccomp: bool,
}

#[cfg(target_os = "linux")]
pub fn enter() -> Result<Report, String> {
    linux::enter()
}

#[cfg(not(target_os = "linux"))]
pub fn enter() -> Result<Report, String> {
    Err("the renderer sandbox is only implemented on Linux; run with --no-sandbox".into())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Report;
    use landlock::{Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetStatus, Scope, ABI};
    use rlimit::Resource;
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};
    use std::collections::BTreeMap;

    const MEMORY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
    const CPU_SECONDS: u64 = 120;

    pub fn enter() -> Result<Report, String> {
        // Touch the bundled fonts now so nothing lazily allocates or reads
        // after the filters are on.
        let _ = crate::font::fonts();

        let set = |r: Resource, v: u64| r.set(v, v).map_err(|e| format!("setrlimit {r:?}: {e}"));
        set(Resource::AS, MEMORY_LIMIT)?;
        set(Resource::CPU, CPU_SECONDS)?;
        set(Resource::CORE, 0)?;

        let landlock = restrict_landlock()?;
        apply_seccomp()?;
        Ok(Report {
            landlock,
            seccomp: true,
        })
    }

    fn restrict_landlock() -> Result<&'static str, String> {
        // Handle every access right this crate knows about and grant none.
        // On older kernels this degrades gracefully (best effort), and seccomp
        // still applies.
        let abi = ABI::V6;
        let status = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .and_then(|r| r.handle_access(AccessNet::from_all(abi)))
            .and_then(|r| r.scope(Scope::from_all(abi)))
            .and_then(|r| r.create())
            .and_then(|r| r.restrict_self())
            .map_err(|e| format!("landlock: {e}"))?;
        Ok(match status.ruleset {
            RulesetStatus::FullyEnforced => "fully enforced",
            RulesetStatus::PartiallyEnforced => "partially enforced",
            RulesetStatus::NotEnforced => "not supported by this kernel",
        })
    }

    /// Syscalls the renderer needs after start-up: pipe I/O, memory
    /// management, threads (for the large-stack worker), time and exiting.
    const ALLOWED: &[i64] = &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_writev,
        libc::SYS_close,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_getrandom,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_clone,
        libc::SYS_clone3,
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_prctl,
        libc::SYS_gettid,
        libc::SYS_getpid,
        libc::SYS_tgkill,
        libc::SYS_membarrier,
    ];

    fn apply_seccomp() -> Result<(), String> {
        let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e| format!("seccomp: {e}"))?;
        let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
            ALLOWED.iter().map(|&n| (n, Vec::new())).collect();
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Errno(libc::EPERM as u32),
            SeccompAction::Allow,
            arch,
        )
        .map_err(|e| format!("seccomp: {e}"))?;
        let prog: BpfProgram = filter.try_into().map_err(|e| format!("seccomp: {e:?}"))?;
        seccompiler::apply_filter_all_threads(&prog).map_err(|e| format!("seccomp: {e}"))
    }
}

/// Try things a compromised renderer would want to do. Each entry is
/// (what, true if it was blocked). Call only after [`enter`].
pub fn probe() -> Vec<(&'static str, bool)> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut buf = String::new();
    out.push((
        "read /etc/passwd",
        std::fs::File::open("/etc/passwd")
            .and_then(|mut f| f.read_to_string(&mut buf))
            .is_err(),
    ));
    out.push((
        "list home directory",
        std::fs::read_dir(std::env::var("HOME").unwrap_or("/root".into())).is_err(),
    ));
    out.push((
        "create a file in /tmp",
        std::fs::write(std::env::temp_dir().join("brickle-probe"), "x").is_err(),
    ));
    out.push((
        "open a TCP connection",
        std::net::TcpStream::connect_timeout(
            &"1.1.1.1:443".parse().unwrap(),
            std::time::Duration::from_secs(2),
        )
        .is_err(),
    ));
    out.push((
        "open a UDP socket",
        std::net::UdpSocket::bind("0.0.0.0:0").is_err(),
    ));
    out.push((
        "run another program",
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("true")
            .status()
            .is_err(),
    ));
    out
}
