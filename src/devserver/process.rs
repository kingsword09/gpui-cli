//! Own a process tree until it has been terminated and its leader reaped.

use std::io;
use std::process::{ChildStderr, ChildStdout, Command, ExitStatus, Stdio};

pub(super) struct OwnedChild {
    #[cfg(unix)]
    child: std::process::Child,
    #[cfg(windows)]
    child: Box<dyn process_wrap::std::ChildWrapper>,
    status: Option<ExitStatus>,
}

impl OwnedChild {
    pub fn spawn(cmd: &mut Command) -> io::Result<Self> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        let child = {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0).spawn()?
        };
        #[cfg(windows)]
        let child = {
            use process_wrap::std::{CommandWrap, JobObject};
            let placeholder = Command::new(cmd.get_program());
            let mut wrapped = CommandWrap::from(std::mem::replace(cmd, placeholder));
            // Assignment happens while suspended, before the child can spawn
            // descendants. The job remains owned even after its leader exits.
            let result = wrapped.wrap(JobObject).spawn();
            *cmd = wrapped.into_command();
            result?
        };
        Ok(Self {
            child,
            status: None,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn stdout(&mut self) -> Option<ChildStdout> {
        #[cfg(unix)]
        return self.child.stdout.take();
        #[cfg(windows)]
        return self.child.stdout().take();
    }

    pub fn stderr(&mut self) -> Option<ChildStderr> {
        #[cfg(unix)]
        return self.child.stderr.take();
        #[cfg(windows)]
        return self.child.stderr().take();
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        #[cfg(unix)]
        if !self.leader_exited()? {
            return Ok(None);
        }
        #[cfg(windows)]
        if self.child.inner_mut().try_wait()?.is_none() {
            return Ok(None);
        }
        self.finish(true).map(Some)
    }

    #[cfg(unix)]
    fn leader_exited(&self) -> io::Result<bool> {
        // Observe without reaping: the zombie leader reserves its PID/PGID
        // until we have killed descendants holding the captured pipes.
        // Killing a cached PGID after reaping could hit a reused ID.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: info is writable and the PID names our unreaped child.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.id() as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        // SAFETY: waitid succeeded; zero si_pid means no child exited.
        Ok(unsafe { info.assume_init().si_pid() } != 0)
    }

    pub fn terminate(&mut self) -> io::Result<ExitStatus> {
        self.finish(false)
    }

    fn finish(&mut self, leader_exited: bool) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        #[cfg(unix)]
        {
            // SAFETY: we have not reaped the leader, so its group ID cannot
            // have been reused. Signal only the group created in spawn().
            if unsafe { libc::kill(-(self.id() as i32), libc::SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                // Darwin can return EPERM for a group containing only its
                // zombie leader. There is nothing left to signal in that case.
                if error.raw_os_error() != Some(libc::ESRCH)
                    && !(error.raw_os_error() == Some(libc::EPERM)
                        && (leader_exited || self.leader_exited()?))
                {
                    return Err(error);
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = leader_exited;
            self.child.start_kill()?;
        }
        #[cfg(unix)]
        let status = self.child.wait()?;
        // Wait only for the leader. Job termination owns descendant cleanup;
        // the output readers independently drain the pipes until EOF. Avoid a
        // blocking job-completion wait after polling has consumed its events.
        #[cfg(windows)]
        let status = self.child.inner_mut().wait()?;
        self.status = Some(status);
        Ok(status)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}
