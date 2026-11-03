//! Locking down the renderer process (Linux).
//!
//! The renderer parses and lays out untrusted web content, so it's the
//! process most likely to be compromised by a bug. Once sandboxed it can only
//! talk to the browser over its stdin/stdout pipes:
//!
//! - **namespaces (best effort):** its own user, network and IPC namespace,
//!   so there is no network to reach even past the other layers.
//! - **rlimits:** bounded memory and CPU time, no core dumps.
//! - **Landlock:** no filesystem access at all, no TCP bind/connect, no
//!   signals or abstract UNIX sockets outside the sandbox.
//! - **seccomp-bpf:** a syscall allowlist; everything else (open, socket,
//!   connect, ...) fails with EPERM. `clone` is limited to threads, memory
//!   can never be made executable, and attacker-only calls (execve, ptrace,
//!   mount, bpf, ...) kill the process.
//!
//! Fonts are compiled into the binary, so the renderer never needs to open a
//! file after it starts.

#[derive(Debug)]
pub struct Report {
    pub landlock: &'static str,
    pub seccomp: bool,
    pub namespaces: String,
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
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };
    use std::collections::BTreeMap;

    const MEMORY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
    const CPU_SECONDS: u64 = 120;

    pub fn enter() -> Result<Report, String> {
        // Touch the bundled fonts now so nothing lazily allocates or reads
        // after the filters are on.
        crate::font::preload();

        let set = |r: Resource, v: u64| r.set(v, v).map_err(|e| format!("setrlimit {r:?}: {e}"));
        set(Resource::AS, MEMORY_LIMIT)?;
        set(Resource::CPU, CPU_SECONDS)?;
        set(Resource::CORE, 0)?;

        // Namespaces must come first: unshare needs a single-threaded process
        // and syscalls the seccomp filter later forbids.
        let namespaces = enter_namespaces();
        let landlock = restrict_landlock()?;
        apply_seccomp()?;
        Ok(Report {
            landlock,
            seccomp: true,
            namespaces,
        })
    }

    /// Best effort: a fresh user, network and IPC namespace. The new network
    /// namespace has no interfaces except a downed loopback, so even code
    /// that got past seccomp and Landlock would find no network at all.
    /// Some systems forbid unprivileged user namespaces (for example Ubuntu
    /// 24.04 / Linux Mint 22 through AppArmor); then this layer is skipped
    /// and the others still apply.
    fn enter_namespaces() -> String {
        use nix::sched::{unshare, CloneFlags};
        match unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNET | CloneFlags::CLONE_NEWIPC) {
            Ok(()) => "network and IPC isolated (own user namespace)".into(),
            Err(e) => format!("unavailable on this system ({e}); other layers still apply"),
        }
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
        libc::SYS_munmap,
        libc::SYS_mremap,
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
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_gettid,
        libc::SYS_getpid,
        libc::SYS_tgkill,
        libc::SYS_membarrier,
    ];

    /// Syscalls that only an attacker would make: the renderer is killed on
    /// the spot rather than just refused.
    const KILL: &[i64] = &[
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_userfaultfd,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_kexec_load,
        libc::SYS_reboot,
        libc::SYS_swapon,
    ];

    fn cond(
        arg: u8,
        len: SeccompCmpArgLen,
        op: SeccompCmpOp,
        value: u64,
    ) -> Result<SeccompCondition, String> {
        SeccompCondition::new(arg, len, op, value).map_err(|e| format!("seccomp: {e}"))
    }

    fn rule(conds: Vec<SeccompCondition>) -> Result<SeccompRule, String> {
        SeccompRule::new(conds).map_err(|e| format!("seccomp: {e}"))
    }

    fn install(
        rules: BTreeMap<i64, Vec<SeccompRule>>,
        mismatch: SeccompAction,
        matched: SeccompAction,
        arch: TargetArch,
    ) -> Result<(), String> {
        let filter =
            SeccompFilter::new(rules, mismatch, matched, arch).map_err(|e| format!("seccomp: {e}"))?;
        let prog: BpfProgram = filter.try_into().map_err(|e| format!("seccomp: {e:?}"))?;
        seccompiler::apply_filter_all_threads(&prog).map_err(|e| format!("seccomp: {e}"))
    }

    /// Three stacked filters. The kernel applies the strictest result
    /// (kill > errno > allow). The allowlist goes on last, because once it is
    /// active no further filters can be installed.
    fn apply_seccomp() -> Result<(), String> {
        use SeccompCmpArgLen::{Dword, Qword};
        let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e| format!("seccomp: {e}"))?;

        // 1. Kill outright on attacker-only syscalls.
        install(
            KILL.iter().map(|&n| (n, Vec::new())).collect(),
            SeccompAction::Allow,
            SeccompAction::KillProcess,
            arch,
        )?;

        // 2. clone3 passes its flags in memory, which seccomp can't inspect.
        //    Report it as unsupported so the C library falls back to clone,
        //    whose flags are checked below.
        install(
            BTreeMap::from([(libc::SYS_clone3, Vec::new())]),
            SeccompAction::Allow,
            SeccompAction::Errno(libc::ENOSYS as u32),
            arch,
        )?;

        // 3. Allowlist: everything else fails with EPERM.
        let mut allow: BTreeMap<i64, Vec<SeccompRule>> = ALLOWED.iter().map(|&n| (n, Vec::new())).collect();
        // (Answered with ENOSYS by filter 2, which takes precedence.)
        allow.insert(libc::SYS_clone3, Vec::new());
        // Threads only: clone must carry CLONE_THREAD (no new processes).
        let thread = libc::CLONE_THREAD as u64;
        allow.insert(
            libc::SYS_clone,
            vec![rule(vec![cond(
                0,
                Qword,
                SeccompCmpOp::MaskedEq(thread),
                thread,
            )?])?],
        );
        // Never executable memory: no JIT, no injected code.
        let exec = libc::PROT_EXEC as u64;
        allow.insert(
            libc::SYS_mmap,
            vec![rule(vec![cond(2, Dword, SeccompCmpOp::MaskedEq(exec), 0)?])?],
        );
        allow.insert(
            libc::SYS_mprotect,
            vec![rule(vec![cond(2, Dword, SeccompCmpOp::MaskedEq(exec), 0)?])?],
        );
        // prctl only for naming threads and memory regions.
        const PR_SET_VMA: u64 = 0x53564d41;
        allow.insert(
            libc::SYS_prctl,
            vec![
                rule(vec![cond(0, Dword, SeccompCmpOp::Eq, libc::PR_SET_NAME as u64)?])?,
                rule(vec![cond(0, Dword, SeccompCmpOp::Eq, PR_SET_VMA)?])?,
            ],
        );
        install(
            allow,
            SeccompAction::Errno(libc::EPERM as u32),
            SeccompAction::Allow,
            arch,
        )
    }
}

/// Things a compromised renderer would want to do. `brickle sandbox-test`
/// runs each in its own sandboxed process, because some of them (like
/// running a program) kill the process outright.
pub const PROBES: &[&str] = &[
    "read /etc/passwd",
    "list home directory",
    "create a file in /tmp",
    "open a TCP connection",
    "open a UDP socket",
    "run another program",
    "replace itself with another program",
];

/// Run one probe (after [`enter`]). Returns true if it was blocked.
pub fn probe(name: &str) -> bool {
    use std::io::Read;
    match name {
        "read /etc/passwd" => {
            let mut buf = String::new();
            std::fs::File::open("/etc/passwd")
                .and_then(|mut f| f.read_to_string(&mut buf))
                .is_err()
        }
        "list home directory" => std::fs::read_dir(std::env::var("HOME").unwrap_or("/root".into())).is_err(),
        "create a file in /tmp" => std::fs::write(std::env::temp_dir().join("brickle-probe"), "x").is_err(),
        "open a TCP connection" => std::net::TcpStream::connect_timeout(
            &"1.1.1.1:443".parse().unwrap(),
            std::time::Duration::from_secs(2),
        )
        .is_err(),
        "open a UDP socket" => std::net::UdpSocket::bind("0.0.0.0:0").is_err(),
        "run another program" => std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("true")
            .status()
            .is_err(),
        // execve in place: the kill filter should end this process (SIGSYS).
        "replace itself with another program" => {
            use std::os::unix::process::CommandExt;
            let _ = std::process::Command::new("/bin/true").exec();
            true
        }
        _ => false,
    }
}
