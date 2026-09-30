//! Cross-process BuildKey coordination for ordinary build outputs.
//!
//! The output lock elects one leader. The coordinator record gives other
//! processes an attempt identity and terminal result to wait for, so they do
//! not independently rerun the same BuildKey build after the lock is released.
//! A successful attempt is accepted only after the ordinary artifact manifest
//! is verified.

use super::build_cache::{BuildCacheLookup, BuildOutputLock, lookup_verified_at_path};
use super::build_key::BuildKey;
use super::output_layout::{
    BUILD_COORDINATOR_LOCK_FILE, BUILD_COORDINATOR_STATE_FILE, BUILD_COORDINATOR_SUBSCRIBERS_DIR,
    BuildOutputLayout, BuildPlatform, PREVIEW_BUILD_COORDINATOR_STATE_FILE,
};
use crate::devserver::session::{BuildCancellationReason, BuildProcessCancellationProbe};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;

pub const BUILD_COORDINATOR_SCHEMA_VERSION: u32 = 1;
pub const BUILD_COORDINATOR_CANCELLED_ERROR: &str = "coordinated BuildKey subscriber cancelled";
pub const BUILD_COORDINATOR_PARTIAL_ERROR: &str =
    "coordinated BuildKey attempt produced partial artifacts";
pub const BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR: &str =
    "coordinated BuildKey leader superseded";
pub const BUILD_COORDINATOR_LEADER_CANCELLED_ERROR: &str =
    "coordinated BuildKey leader cancelled without subscribers";
const BUILD_COORDINATOR_BUILD_KIND: &str = "build";
const BUILD_COORDINATOR_PREVIEW_KIND: &str = "preview";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const LEADER_ABANDONED_ERROR: &str = "build leader exited before publishing a terminal state";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCoordinatorState {
    Building,
    Succeeded,
    Failed,
    Partial,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildCoordinatorRecord {
    pub schema_version: u32,
    pub kind: String,
    pub platform: BuildPlatform,
    pub key_hash: String,
    pub attempt_id: String,
    pub owner_id: String,
    pub pid: u32,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub state: BuildCoordinatorState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildCoordinatorRole {
    Leader,
    Follower,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildCoordinatorCancellationReason {
    Superseded,
    CallerCancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCoordinatorOutcome {
    pub role: BuildCoordinatorRole,
    pub attempt_id: String,
}

/// A short-lived reference held by a waiting caller. The file is intentionally
/// separate from the state record so a leader never has to rewrite subscriber
/// count while it is running the compiler.
struct BuildCoordinatorSubscription {
    path: PathBuf,
    _file: File,
}

#[derive(Clone)]
pub struct BuildCoordinatorLeaderControl {
    inner: Arc<BuildCoordinatorLeaderControlInner>,
}

struct BuildCoordinatorLeaderControlInner {
    layout: BuildOutputLayout,
    building: BuildCoordinatorRecord,
    cancellation_reason:
        Arc<dyn Fn() -> Result<Option<BuildCoordinatorCancellationReason>> + Send + Sync>,
    subscription: Mutex<Option<BuildCoordinatorSubscription>>,
    terminal_published: AtomicBool,
    terminal_reason: Mutex<Option<BuildCoordinatorCancellationReason>>,
}

impl BuildCoordinatorLeaderControl {
    fn new<C>(
        layout: &BuildOutputLayout,
        building: BuildCoordinatorRecord,
        subscription: BuildCoordinatorSubscription,
        cancellation_reason: C,
    ) -> Self
    where
        C: Fn() -> Result<Option<BuildCoordinatorCancellationReason>> + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(BuildCoordinatorLeaderControlInner {
                layout: layout.clone(),
                building,
                cancellation_reason: Arc::new(cancellation_reason),
                subscription: Mutex::new(Some(subscription)),
                terminal_published: AtomicBool::new(false),
                terminal_reason: Mutex::new(None),
            }),
        }
    }

    fn finish(&self, mut result: Result<()>) -> Result<(Result<()>, Option<anyhow::Error>)> {
        if self.inner.terminal_published.load(Ordering::SeqCst) {
            let reason = *self
                .inner
                .terminal_reason
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            return Ok(match reason {
                Some(BuildCoordinatorCancellationReason::Superseded) => (
                    Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)),
                    Some(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)),
                ),
                Some(BuildCoordinatorCancellationReason::CallerCancelled) => (
                    Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR)),
                    Some(anyhow::anyhow!(BUILD_COORDINATOR_CANCELLED_ERROR)),
                ),
                None => (result, None),
            });
        }

        let state_lock = open_state_lock(&self.inner.layout.root)?;
        let cancellation = match (self.inner.cancellation_reason)() {
            Ok(reason) => reason,
            Err(error) => {
                result = Err(error).context("checking BuildKey leader cancellation");
                None
            }
        };
        let superseded = result
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string() == BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR);
        let mut caller_error = None;
        let mut terminal_reason = cancellation;
        if superseded {
            terminal_reason = Some(BuildCoordinatorCancellationReason::Superseded);
        }
        match cancellation {
            Some(BuildCoordinatorCancellationReason::Superseded) => {
                caller_error = Some(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
                result = Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
                self.release_subscription();
            }
            Some(BuildCoordinatorCancellationReason::CallerCancelled) => {
                caller_error = Some(if superseded {
                    anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)
                } else {
                    anyhow::anyhow!(BUILD_COORDINATOR_CANCELLED_ERROR)
                });
                self.release_subscription();
                if active_subscriber_count_locked(
                    &self.inner.layout,
                    &self.inner.building.attempt_id,
                )? == 0
                    && !superseded
                {
                    result = Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR));
                }
            }
            None if superseded => {
                terminal_reason = Some(BuildCoordinatorCancellationReason::Superseded);
                caller_error = Some(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
                result = Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
                self.release_subscription();
            }
            None => {}
        }

        let terminal = self.terminal_record(result.as_ref().err());
        let state_result = write_record_locked(&self.inner.layout, &terminal);
        *self
            .inner
            .terminal_reason
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = terminal_reason;
        self.inner.terminal_published.store(true, Ordering::SeqCst);
        if terminal_reason.is_none() {
            self.release_subscription();
        }
        drop(state_lock);

        match state_result {
            Ok(()) => Ok((result, caller_error)),
            Err(error) => match caller_error {
                Some(caller_error) => Err(caller_error)
                    .context(format!("publishing BuildKey coordinator state: {error:#}")),
                None => Err(error),
            },
        }
    }

    fn release_subscription(&self) {
        self.inner
            .subscription
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn terminal_record(&self, error: Option<&anyhow::Error>) -> BuildCoordinatorRecord {
        let mut record = self.inner.building.clone();
        record.finished_at_ms = Some(timestamp_ms());
        record.state = terminal_state(error);
        record.error = error.map(format_error);
        record
    }
}

impl BuildProcessCancellationProbe for BuildCoordinatorLeaderControl {
    fn should_abort(&self) -> Result<Option<BuildCancellationReason>> {
        if self.inner.terminal_published.load(Ordering::SeqCst) {
            return Ok(self
                .inner
                .terminal_reason
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .map(|reason| match reason {
                    BuildCoordinatorCancellationReason::CallerCancelled => {
                        BuildCancellationReason::CallerCancelled
                    }
                    BuildCoordinatorCancellationReason::Superseded => {
                        BuildCancellationReason::Superseded
                    }
                }));
        }
        let Some(reason) = (self.inner.cancellation_reason)()? else {
            return Ok(None);
        };
        let state_lock = open_state_lock(&self.inner.layout.root)?;
        self.release_subscription();
        let remaining =
            active_subscriber_count_locked(&self.inner.layout, &self.inner.building.attempt_id)?;
        drop(state_lock);
        Ok(
            (reason == BuildCoordinatorCancellationReason::Superseded || remaining == 0).then_some(
                match reason {
                    BuildCoordinatorCancellationReason::CallerCancelled => {
                        BuildCancellationReason::CallerCancelled
                    }
                    BuildCoordinatorCancellationReason::Superseded => {
                        BuildCancellationReason::Superseded
                    }
                },
            ),
        )
    }

    fn terminate_if_cancelled(
        &self,
        terminate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<BuildCancellationReason>> {
        if self.inner.terminal_published.load(Ordering::SeqCst) {
            return self.should_abort();
        }
        let Some(reason) = self.should_abort()? else {
            return Ok(None);
        };

        let state_lock = open_state_lock(&self.inner.layout.root)?;
        self.release_subscription();
        terminate()?;
        let (error, terminal_reason) = match reason {
            BuildCancellationReason::Superseded => (
                BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR,
                BuildCoordinatorCancellationReason::Superseded,
            ),
            BuildCancellationReason::CallerCancelled => (
                BUILD_COORDINATOR_LEADER_CANCELLED_ERROR,
                BuildCoordinatorCancellationReason::CallerCancelled,
            ),
        };
        let terminal_error = anyhow::anyhow!(error);
        let terminal = self.terminal_record(Some(&terminal_error));
        write_record_locked(&self.inner.layout, &terminal)?;
        *self
            .inner
            .terminal_reason
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(terminal_reason);
        self.inner.terminal_published.store(true, Ordering::SeqCst);
        drop(state_lock);
        Ok(Some(reason))
    }
}

impl Drop for BuildCoordinatorSubscription {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Runs one ordinary BuildKey build or joins the already-running attempt.
///
/// The closure is invoked only by the elected leader. Followers wait for the
/// leader's terminal state and verify the published artifact manifest before
/// returning. If the leader disappears without publishing a terminal state,
/// a follower can become the next leader and execute the same closure.
pub fn coordinate_build<F>(
    layout: &BuildOutputLayout,
    key: &BuildKey,
    build: F,
) -> Result<BuildCoordinatorOutcome>
where
    F: FnOnce() -> Result<()>,
{
    coordinate_build_with_verifier_kind(
        layout,
        key.key_hash(),
        BUILD_COORDINATOR_BUILD_KIND,
        build,
        verify_completed_output,
        || Ok(false),
        BuildCoordinatorCancellationReason::Superseded,
    )
}

/// Coordinates one BuildKey attempt with a caller-provided completion
/// verifier. Preview artifacts use this for their preview manifest while
/// ordinary build/run uses the regular artifact manifest.
pub fn coordinate_build_with_verifier<F, V>(
    layout: &BuildOutputLayout,
    key_hash: &str,
    build: F,
    verify: V,
) -> Result<BuildCoordinatorOutcome>
where
    F: FnOnce() -> Result<()>,
    V: Fn(&BuildOutputLayout, &str) -> Result<()>,
{
    coordinate_build_with_verifier_kind(
        layout,
        key_hash,
        BUILD_COORDINATOR_BUILD_KIND,
        build,
        verify,
        || Ok(false),
        BuildCoordinatorCancellationReason::Superseded,
    )
}

/// Coordinates a preview artifact attempt separately from ordinary build/run
/// artifacts that share the same output root.
pub fn coordinate_preview_build_with_verifier<F, V>(
    layout: &BuildOutputLayout,
    key_hash: &str,
    build: F,
    verify: V,
) -> Result<BuildCoordinatorOutcome>
where
    F: FnOnce() -> Result<()>,
    V: Fn(&BuildOutputLayout, &str) -> Result<()>,
{
    coordinate_build_with_verifier_kind(
        layout,
        key_hash,
        BUILD_COORDINATOR_PREVIEW_KIND,
        build,
        verify,
        || Ok(false),
        BuildCoordinatorCancellationReason::Superseded,
    )
}

/// Coordinates a preview attempt while allowing a waiting caller to release
/// its subscriber when the caller no longer needs the result. The leader is
/// not cancelled by a follower releasing its subscription.
pub fn coordinate_preview_build_with_verifier_and_cancel<F, V, C>(
    layout: &BuildOutputLayout,
    key_hash: &str,
    build: F,
    verify: V,
    cancel: C,
) -> Result<BuildCoordinatorOutcome>
where
    F: FnOnce() -> Result<()>,
    V: Fn(&BuildOutputLayout, &str) -> Result<()>,
    C: Fn() -> Result<bool>,
{
    coordinate_build_with_verifier_kind(
        layout,
        key_hash,
        BUILD_COORDINATOR_PREVIEW_KIND,
        build,
        verify,
        cancel,
        BuildCoordinatorCancellationReason::Superseded,
    )
}

/// Coordinates a preview attempt whose leader may stop needing the result.
/// If follower references remain, the terminal result stays reusable for them;
/// only the cancelled leader returns `BUILD_COORDINATOR_CANCELLED_ERROR`.
pub fn coordinate_preview_build_with_verifier_and_caller_cancel<F, V, C>(
    layout: &BuildOutputLayout,
    key_hash: &str,
    build: F,
    verify: V,
    cancel: C,
) -> Result<BuildCoordinatorOutcome>
where
    F: FnOnce() -> Result<()>,
    V: Fn(&BuildOutputLayout, &str) -> Result<()>,
    C: Fn() -> Result<bool>,
{
    coordinate_build_with_verifier_kind(
        layout,
        key_hash,
        BUILD_COORDINATOR_PREVIEW_KIND,
        build,
        verify,
        cancel,
        BuildCoordinatorCancellationReason::CallerCancelled,
    )
}

/// Coordinates a preview build with a leader control that can fence process
/// termination against active subscriber references.
pub fn coordinate_preview_build_with_leader_control<F, V, C>(
    layout: &BuildOutputLayout,
    key_hash: &str,
    build: F,
    verify: V,
    cancellation_reason: C,
) -> Result<BuildCoordinatorOutcome>
where
    F: FnOnce(&BuildCoordinatorLeaderControl) -> Result<()>,
    V: Fn(&BuildOutputLayout, &str) -> Result<()>,
    C: Fn() -> Result<Option<BuildCoordinatorCancellationReason>> + Send + Sync + 'static,
{
    let cancellation_reason = Arc::new(cancellation_reason);
    let mut build = Some(build);
    let cancel_waiter = || Ok((cancellation_reason)()?.is_some());

    loop {
        if cancel_waiter()? {
            bail!(BUILD_COORDINATOR_CANCELLED_ERROR);
        }
        if let Some(record) = read_record(layout, BUILD_COORDINATOR_PREVIEW_KIND)?
            && record.platform == layout.platform
            && record.kind == BUILD_COORDINATOR_PREVIEW_KIND
            && record.key_hash == key_hash
        {
            match record.state {
                BuildCoordinatorState::Building => {
                    match wait_for_attempt(
                        layout,
                        key_hash,
                        BUILD_COORDINATOR_PREVIEW_KIND,
                        &record,
                        &verify,
                        &cancel_waiter,
                    )? {
                        WaitOutcome::Completed(outcome) => return Ok(outcome),
                        WaitOutcome::Abandoned => continue,
                    }
                }
                BuildCoordinatorState::Succeeded => {
                    if verify(layout, key_hash).is_ok() {
                        return Ok(BuildCoordinatorOutcome {
                            role: BuildCoordinatorRole::Follower,
                            attempt_id: record.attempt_id,
                        });
                    }
                }
                BuildCoordinatorState::Failed => {
                    if !matches!(
                        record.error.as_deref(),
                        Some(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)
                            | Some(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR)
                    ) && active_subscriber_count(layout, &record.attempt_id)? > 0
                    {
                        bail!(
                            "coordinated BuildKey build failed: {}",
                            record
                                .error
                                .unwrap_or_else(|| "build failed without an error".into())
                        );
                    }
                }
                BuildCoordinatorState::Partial => {}
                BuildCoordinatorState::Cancelled => {}
            }
        }

        let Some(lock) = BuildOutputLock::try_acquire(layout)? else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };

        let attempt_id = unique_id("attempt");
        let owner_id = unique_id("coordinator");
        let started_at_ms = timestamp_ms();
        let building = BuildCoordinatorRecord {
            schema_version: BUILD_COORDINATOR_SCHEMA_VERSION,
            kind: BUILD_COORDINATOR_PREVIEW_KIND.to_owned(),
            platform: layout.platform,
            key_hash: key_hash.to_owned(),
            attempt_id: attempt_id.clone(),
            owner_id,
            pid: process::id(),
            started_at_ms,
            finished_at_ms: None,
            state: BuildCoordinatorState::Building,
            error: None,
        };
        let subscription = subscribe(layout, &attempt_id)?;
        write_record(layout, &building)?;
        let control_cancellation_reason = Arc::clone(&cancellation_reason);
        let control =
            BuildCoordinatorLeaderControl::new(layout, building, subscription, move || {
                control_cancellation_reason()
            });

        let build_once = build
            .take()
            .expect("BuildKey coordinator closure is consumed once");
        let result = match build_once(&control) {
            Ok(()) => verify(layout, key_hash),
            Err(error) => Err(error),
        };
        let result = match result {
            Err(error) if format!("{error:#}").contains("superseded") => {
                Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR))
            }
            result => result,
        };
        let (result, caller_error) = control.finish(result)?;
        drop(lock);
        if let Some(error) = caller_error {
            return Err(error);
        }
        result?;
        return Ok(BuildCoordinatorOutcome {
            role: BuildCoordinatorRole::Leader,
            attempt_id,
        });
    }
}

fn coordinate_build_with_verifier_kind<F, V, C>(
    layout: &BuildOutputLayout,
    key_hash: &str,
    kind: &str,
    build: F,
    verify: V,
    cancel: C,
    cancellation_reason: BuildCoordinatorCancellationReason,
) -> Result<BuildCoordinatorOutcome>
where
    F: FnOnce() -> Result<()>,
    V: Fn(&BuildOutputLayout, &str) -> Result<()>,
    C: Fn() -> Result<bool>,
{
    let mut build = Some(build);

    loop {
        if cancel()? {
            bail!(BUILD_COORDINATOR_CANCELLED_ERROR);
        }
        if let Some(record) = read_record(layout, kind)?
            && record.platform == layout.platform
            && record.kind == kind
            && record.key_hash == key_hash
        {
            match record.state {
                BuildCoordinatorState::Building => {
                    match wait_for_attempt(layout, key_hash, kind, &record, &verify, &cancel)? {
                        WaitOutcome::Completed(outcome) => return Ok(outcome),
                        WaitOutcome::Abandoned => continue,
                    }
                }
                BuildCoordinatorState::Succeeded => {
                    if verify(layout, key_hash).is_ok() {
                        return Ok(BuildCoordinatorOutcome {
                            role: BuildCoordinatorRole::Follower,
                            attempt_id: record.attempt_id,
                        });
                    }
                }
                BuildCoordinatorState::Failed => {
                    if !matches!(
                        record.error.as_deref(),
                        Some(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)
                            | Some(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR)
                    ) && active_subscriber_count(layout, &record.attempt_id)? > 0
                    {
                        bail!(
                            "coordinated BuildKey build failed: {}",
                            record
                                .error
                                .unwrap_or_else(|| "build failed without an error".into())
                        );
                    }
                }
                BuildCoordinatorState::Partial => {}
                BuildCoordinatorState::Cancelled => {}
            }
        }

        let Some(lock) = BuildOutputLock::try_acquire(layout)? else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };

        let attempt_id = unique_id("attempt");
        let owner_id = unique_id("coordinator");
        let started_at_ms = timestamp_ms();
        // The leader is also a live subscriber. Keeping this OS-locked file
        // until the terminal state is published prevents a concurrent caller
        // from treating a still-returning failed leader as abandoned.
        let mut leader_subscription = Some(subscribe(layout, &attempt_id)?);
        write_record(
            layout,
            &BuildCoordinatorRecord {
                schema_version: BUILD_COORDINATOR_SCHEMA_VERSION,
                kind: kind.to_owned(),
                platform: layout.platform,
                key_hash: key_hash.to_string(),
                attempt_id: attempt_id.clone(),
                owner_id: owner_id.clone(),
                pid: process::id(),
                started_at_ms,
                finished_at_ms: None,
                state: BuildCoordinatorState::Building,
                error: None,
            },
        )?;

        let build_once = build
            .take()
            .expect("BuildKey coordinator closure is consumed once");
        let result = build_once();
        let result = match result {
            Ok(()) => verify(layout, key_hash),
            Err(error) => Err(error),
        };
        let mut result = match result {
            Err(error) if format!("{error:#}").contains("superseded") => {
                Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR))
            }
            result => result,
        };
        let superseded = result
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string() == BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR);
        let cancellation_requested = match cancel() {
            Ok(cancelled) => cancelled,
            Err(error) => {
                result = Err(error).context("checking BuildKey leader cancellation");
                false
            }
        };
        let mut caller_error = None;
        let mut terminal_state_lock = None;
        if cancellation_requested {
            match cancellation_reason {
                BuildCoordinatorCancellationReason::Superseded => {
                    caller_error = Some(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
                    terminal_state_lock = Some(open_state_lock(&layout.root)?);
                    drop(leader_subscription.take());
                    result = Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
                }
                BuildCoordinatorCancellationReason::CallerCancelled => {
                    caller_error = Some(if superseded {
                        anyhow::anyhow!(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)
                    } else {
                        anyhow::anyhow!(BUILD_COORDINATOR_CANCELLED_ERROR)
                    });
                    let state_lock = open_state_lock(&layout.root)?;
                    drop(leader_subscription.take());
                    let remaining_subscribers =
                        active_subscriber_count_locked(layout, &attempt_id)?;
                    if remaining_subscribers == 0 && !superseded {
                        result = Err(anyhow::anyhow!(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR));
                    }
                    terminal_state_lock = Some(state_lock);
                }
            }
        }

        let terminal = BuildCoordinatorRecord {
            schema_version: BUILD_COORDINATOR_SCHEMA_VERSION,
            kind: kind.to_owned(),
            platform: layout.platform,
            key_hash: key_hash.to_string(),
            attempt_id: attempt_id.clone(),
            owner_id,
            pid: process::id(),
            started_at_ms,
            finished_at_ms: Some(timestamp_ms()),
            state: terminal_state(result.as_ref().err()),
            error: result.as_ref().err().map(format_error),
        };
        let state_result = match terminal_state_lock.as_ref() {
            Some(_) => write_record_locked(layout, &terminal),
            None => write_record(layout, &terminal),
        };
        drop(lock);
        drop(terminal_state_lock);
        drop(leader_subscription);

        if let Some(error) = caller_error {
            return match state_result {
                Ok(()) => Err(error),
                Err(state_error) => Err(error).context(format!(
                    "publishing BuildKey coordinator state: {state_error:#}"
                )),
            };
        }

        match (result, state_result) {
            (Ok(()), Ok(())) => {
                return Ok(BuildCoordinatorOutcome {
                    role: BuildCoordinatorRole::Leader,
                    attempt_id,
                });
            }
            (Err(error), Ok(())) => return Err(error),
            (Ok(()), Err(error)) => {
                return Err(error).context("publishing successful BuildKey coordinator state");
            }
            (Err(error), Err(state_error)) => {
                return Err(error).context(format!(
                    "publishing failed BuildKey coordinator state: {state_error:#}"
                ));
            }
        }
    }
}

enum WaitOutcome {
    Completed(BuildCoordinatorOutcome),
    Abandoned,
}

fn wait_for_attempt(
    layout: &BuildOutputLayout,
    key_hash: &str,
    kind: &str,
    expected: &BuildCoordinatorRecord,
    verify: &impl Fn(&BuildOutputLayout, &str) -> Result<()>,
    cancel: &impl Fn() -> Result<bool>,
) -> Result<WaitOutcome> {
    let Some(subscription) = subscribe_if_building(layout, kind, key_hash, expected)? else {
        return Ok(WaitOutcome::Abandoned);
    };

    loop {
        if cancel()? {
            drop(subscription);
            bail!(BUILD_COORDINATOR_CANCELLED_ERROR);
        }
        match read_record(layout, kind)? {
            Some(record)
                if record.platform == layout.platform
                    && record.kind == kind
                    && record.key_hash == key_hash
                    && record.attempt_id == expected.attempt_id =>
            {
                match record.state {
                    BuildCoordinatorState::Building => {
                        if let Some(lock) = BuildOutputLock::try_acquire(layout)? {
                            let abandoned = BuildCoordinatorRecord {
                                schema_version: BUILD_COORDINATOR_SCHEMA_VERSION,
                                kind: kind.to_owned(),
                                platform: layout.platform,
                                key_hash: key_hash.to_string(),
                                attempt_id: record.attempt_id,
                                owner_id: record.owner_id,
                                pid: record.pid,
                                started_at_ms: record.started_at_ms,
                                finished_at_ms: Some(timestamp_ms()),
                                state: BuildCoordinatorState::Failed,
                                error: Some(LEADER_ABANDONED_ERROR.into()),
                            };
                            write_record(layout, &abandoned)?;
                            drop(lock);
                            drop(subscription);
                            return Ok(WaitOutcome::Abandoned);
                        }
                        thread::sleep(POLL_INTERVAL);
                    }
                    BuildCoordinatorState::Succeeded => {
                        verify(layout, key_hash)?;
                        drop(subscription);
                        return Ok(WaitOutcome::Completed(BuildCoordinatorOutcome {
                            role: BuildCoordinatorRole::Follower,
                            attempt_id: record.attempt_id,
                        }));
                    }
                    BuildCoordinatorState::Failed => {
                        if matches!(
                            record.error.as_deref(),
                            Some(LEADER_ABANDONED_ERROR)
                                | Some(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)
                                | Some(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR)
                        ) {
                            drop(subscription);
                            return Ok(WaitOutcome::Abandoned);
                        }
                        let reason = record
                            .error
                            .unwrap_or_else(|| "coordinated build failed without an error".into());
                        drop(subscription);
                        bail!("coordinated BuildKey build failed: {reason}");
                    }
                    BuildCoordinatorState::Partial => {
                        drop(subscription);
                        return Ok(WaitOutcome::Abandoned);
                    }
                    BuildCoordinatorState::Cancelled => {
                        drop(subscription);
                        return Ok(WaitOutcome::Abandoned);
                    }
                }
            }
            Some(_) | None => {
                if let Some(lock) = BuildOutputLock::try_acquire(layout)? {
                    if let Some(record) = read_record(layout, kind)?
                        && record.state == BuildCoordinatorState::Building
                        && record.platform == layout.platform
                        && record.kind == kind
                        && record.key_hash == key_hash
                    {
                        let abandoned = BuildCoordinatorRecord {
                            schema_version: BUILD_COORDINATOR_SCHEMA_VERSION,
                            kind: kind.to_owned(),
                            platform: layout.platform,
                            key_hash: key_hash.to_string(),
                            attempt_id: record.attempt_id,
                            owner_id: record.owner_id,
                            pid: record.pid,
                            started_at_ms: record.started_at_ms,
                            finished_at_ms: Some(timestamp_ms()),
                            state: BuildCoordinatorState::Failed,
                            error: Some(LEADER_ABANDONED_ERROR.into()),
                        };
                        write_record(layout, &abandoned)?;
                    }
                    drop(lock);
                    drop(subscription);
                    return Ok(WaitOutcome::Abandoned);
                }
                thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

fn verify_completed_output(layout: &BuildOutputLayout, key_hash: &str) -> Result<()> {
    match lookup_verified_at_path(
        &layout.artifact_manifest_path(),
        &layout.root,
        layout.platform,
        key_hash,
    ) {
        BuildCacheLookup::Hit(_) => Ok(()),
        BuildCacheLookup::Miss(reason) => {
            bail!("coordinated build completed without a verified artifact: {reason}")
        }
    }
}

fn subscribe(layout: &BuildOutputLayout, attempt_id: &str) -> Result<BuildCoordinatorSubscription> {
    let _state_lock = lock_coordinator_state(&layout.root, true)?
        .expect("creating a coordinator state lock returns its guard");
    create_subscription(layout, attempt_id)
}

fn subscribe_if_building(
    layout: &BuildOutputLayout,
    kind: &str,
    key_hash: &str,
    expected: &BuildCoordinatorRecord,
) -> Result<Option<BuildCoordinatorSubscription>> {
    let _state_lock = lock_coordinator_state(&layout.root, true)?
        .expect("creating a coordinator state lock returns its guard");
    let Some(record) = read_record(layout, kind)? else {
        return Ok(None);
    };
    if record.platform != layout.platform
        || record.kind != kind
        || record.key_hash != key_hash
        || record.attempt_id != expected.attempt_id
        || record.state != BuildCoordinatorState::Building
    {
        return Ok(None);
    }
    create_subscription(layout, &expected.attempt_id).map(Some)
}

fn create_subscription(
    layout: &BuildOutputLayout,
    attempt_id: &str,
) -> Result<BuildCoordinatorSubscription> {
    let directory = layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR);
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "creating BuildKey coordinator subscribers directory {}",
            directory.display()
        )
    })?;
    let attempt_segment = encode_attempt_id(attempt_id);
    let path = directory.join(format!(
        "{}-{}-{}.json",
        attempt_segment,
        process::id(),
        unique_id("subscriber")
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| {
            format!(
                "creating BuildKey coordinator subscription {}",
                path.display()
            )
        })?;
    file.lock_exclusive().with_context(|| {
        format!(
            "locking BuildKey coordinator subscription {}",
            path.display()
        )
    })?;
    let record = serde_json::json!({
        "schema_version": BUILD_COORDINATOR_SCHEMA_VERSION,
        "attempt_id": attempt_id,
        "pid": process::id(),
        "joined_at_ms": timestamp_ms(),
    });
    serde_json::to_writer_pretty(&mut file, &record)
        .context("serializing BuildKey coordinator subscription")?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(BuildCoordinatorSubscription { path, _file: file })
}

fn encode_attempt_id(attempt_id: &str) -> String {
    attempt_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn active_subscriber_count(layout: &BuildOutputLayout, attempt_id: &str) -> Result<usize> {
    let _state_lock = lock_coordinator_state(&layout.root, true)?
        .expect("creating a coordinator state lock returns its guard");
    active_subscriber_count_locked(layout, attempt_id)
}

fn active_subscriber_count_locked(layout: &BuildOutputLayout, attempt_id: &str) -> Result<usize> {
    let directory = layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "scanning BuildKey coordinator subscribers {}",
                    directory.display()
                )
            });
        }
    };
    let prefix = format!("{}-", encode_attempt_id(attempt_id));
    let mut count = 0;

    for entry in entries {
        let path = entry?.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("checking subscriber {}", path.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }

        let probe = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(probe) => probe,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                count += 1;
                continue;
            }
        };
        match probe.try_lock_exclusive() {
            Ok(()) => {
                let _ = probe.unlock();
                let _ = fs::remove_file(&path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => count += 1,
            Err(_) => count += 1,
        }
    }
    Ok(count)
}

fn coordinator_state_path(layout: &BuildOutputLayout, kind: &str) -> PathBuf {
    let file = match kind {
        BUILD_COORDINATOR_PREVIEW_KIND => PREVIEW_BUILD_COORDINATOR_STATE_FILE,
        _ => BUILD_COORDINATOR_STATE_FILE,
    };
    layout.root.join(file)
}

fn read_record(layout: &BuildOutputLayout, kind: &str) -> Result<Option<BuildCoordinatorRecord>> {
    let path = coordinator_state_path(layout, kind);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!(
                "BuildKey coordinator state is not a regular file: {}",
                path.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("checking BuildKey coordinator state {}", path.display())
            });
        }
    }

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading BuildKey coordinator state {}", path.display()));
        }
    };
    let record = match serde_json::from_slice::<BuildCoordinatorRecord>(&bytes) {
        Ok(record) => record,
        Err(_) => return Ok(None),
    };
    if record.schema_version != BUILD_COORDINATOR_SCHEMA_VERSION {
        return Ok(None);
    }
    Ok(Some(record))
}

fn write_record(layout: &BuildOutputLayout, record: &BuildCoordinatorRecord) -> Result<()> {
    let state_lock = open_state_lock(&layout.root)?;
    let result = write_record_locked(layout, record);
    drop(state_lock);
    result
}

fn write_record_locked(layout: &BuildOutputLayout, record: &BuildCoordinatorRecord) -> Result<()> {
    let path = coordinator_state_path(layout, &record.kind);
    validate_regular_or_missing(&path, "BuildKey coordinator state")?;
    let parent = path
        .parent()
        .context("BuildKey coordinator state has no parent")?;
    let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "creating temporary BuildKey coordinator state in {}",
            parent.display()
        )
    })?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), record)
        .context("serializing BuildKey coordinator state")?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| {
        anyhow::anyhow!(
            "publishing BuildKey coordinator state {}: {}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

pub(super) fn lock_coordinator_state(root: &Path, create: bool) -> Result<Option<File>> {
    let path = root.join(BUILD_COORDINATOR_LOCK_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!(
                "BuildKey coordinator lock is not a regular file: {}",
                path.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
            return Ok(None);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("checking BuildKey coordinator lock {}", path.display()));
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening BuildKey coordinator lock {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking BuildKey coordinator state {}", root.display()))?;
    Ok(Some(file))
}

fn open_state_lock(root: &Path) -> Result<File> {
    lock_coordinator_state(root, true)?.context("creating BuildKey coordinator state lock")
}

fn validate_regular_or_missing(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("{label} is not a regular file: {}", path.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("checking {label} {}", path.display())),
    }
}

fn unique_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}-{}",
        process::id(),
        timestamp_nanos(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn format_error(error: &anyhow::Error) -> String {
    let mut message = format!("{error:#}");
    if message.len() > 4096 {
        message.truncate(4096);
        message.push('…');
    }
    message
}

fn terminal_state(error: Option<&anyhow::Error>) -> BuildCoordinatorState {
    let Some(error) = error else {
        return BuildCoordinatorState::Succeeded;
    };
    let has_marker = |marker: &str| error.chain().any(|cause| cause.to_string() == marker);
    if has_marker(BUILD_COORDINATOR_PARTIAL_ERROR) {
        BuildCoordinatorState::Partial
    } else if has_marker(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR) {
        BuildCoordinatorState::Cancelled
    } else {
        BuildCoordinatorState::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::build_key::BuildKeyMaterial;
    use crate::runner::build_manifest::BuildArtifactManifest;
    use std::env;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key() -> BuildKey {
        BuildKey::new(BuildKeyMaterial {
            source_manifest_hash: "source".into(),
            cargo_lock_hash: "lock".into(),
            target_triple: "x86_64-unknown-linux-gnu".into(),
            profile: "dev".into(),
            features: Vec::new(),
            abi: None,
            native_config_hash: "native".into(),
            toolchain_fingerprint: "toolchain".into(),
            relevant_env_hash: "env".into(),
            preview_registry_hash: "registry".into(),
        })
        .unwrap()
    }

    fn layout(base: &Path) -> BuildOutputLayout {
        BuildOutputLayout::for_key(base, &key(), BuildPlatform::Desktop).unwrap()
    }

    fn publish_fake_artifact(layout: &BuildOutputLayout) {
        let executable = layout.cargo_target_dir.join("debug/app");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"complete binary").unwrap();
        let entry = executable.strip_prefix(&layout.root).unwrap().to_owned();
        BuildArtifactManifest::capture(layout, &[entry])
            .unwrap()
            .write_atomic(&layout.artifact_manifest_path())
            .unwrap();
    }

    #[test]
    fn same_key_builders_share_one_in_flight_attempt() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let count = Arc::new(AtomicUsize::new(0));
        let first_layout = layout.clone();
        let first_key = key.clone();
        let first_count = count.clone();
        let first = std::thread::spawn(move || {
            coordinate_build(&first_layout, &first_key, || {
                assert_eq!(first_count.fetch_add(1, Ordering::SeqCst), 0);
                std::thread::sleep(Duration::from_millis(200));
                publish_fake_artifact(&first_layout);
                Ok(())
            })
            .unwrap()
        });

        let state_path = layout.root.join(BUILD_COORDINATOR_STATE_FILE);
        for _ in 0..100 {
            if state_path.is_file() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state_path.is_file());

        let second_layout = layout.clone();
        let second_key = key.clone();
        let second = std::thread::spawn(move || {
            coordinate_build(&second_layout, &second_key, || {
                panic!("follower must not execute the build closure");
            })
            .unwrap()
        });

        let first_outcome = first.join().unwrap();
        let second_outcome = second.join().unwrap();
        assert_eq!(first_outcome.role, BuildCoordinatorRole::Leader);
        assert_eq!(second_outcome.role, BuildCoordinatorRole::Follower);
        assert_eq!(first_outcome.attempt_id, second_outcome.attempt_id);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR).is_dir());
        assert!(
            fs::read_dir(layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn leader_holds_a_subscriber_reference_until_terminal_state() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_layout = layout.clone();
        let first_key = key.clone();
        let leader = std::thread::spawn(move || {
            coordinate_build(&first_layout, &first_key, || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                publish_fake_artifact(&first_layout);
                Ok(())
            })
            .unwrap()
        });

        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let subscribers = layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR);
        assert_eq!(fs::read_dir(&subscribers).unwrap().count(), 1);
        let attempt_id = read_record(&layout, BUILD_COORDINATOR_BUILD_KIND)
            .unwrap()
            .unwrap()
            .attempt_id;
        assert_eq!(active_subscriber_count(&layout, &attempt_id).unwrap(), 1);
        release_tx.send(()).unwrap();
        assert_eq!(leader.join().unwrap().role, BuildCoordinatorRole::Leader);
        assert!(fs::read_dir(subscribers).unwrap().next().is_none());
        assert_eq!(active_subscriber_count(&layout, &attempt_id).unwrap(), 0);
    }

    #[test]
    fn cancelled_preview_follower_releases_subscriber_without_stopping_leader() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let (leader_started_tx, leader_started_rx) = std::sync::mpsc::channel();
        let (allow_leader_tx, allow_leader_rx) = std::sync::mpsc::channel();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_layout = layout.clone();
        let first_key_hash = key.key_hash().to_owned();
        let first = std::thread::spawn(move || {
            coordinate_preview_build_with_verifier(
                &first_layout,
                &first_key_hash,
                || {
                    leader_started_tx.send(()).unwrap();
                    allow_leader_rx.recv().unwrap();
                    publish_fake_artifact(&first_layout);
                    Ok(())
                },
                verify_completed_output,
            )
            .unwrap()
        });

        leader_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let attempt_id = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap()
            .attempt_id;
        let second_layout = layout.clone();
        let second_key_hash = key.key_hash().to_owned();
        let second_cancelled = cancelled.clone();
        let second = std::thread::spawn(move || {
            coordinate_preview_build_with_verifier_and_cancel(
                &second_layout,
                &second_key_hash,
                || panic!("cancelled follower must not execute the build closure"),
                verify_completed_output,
                || Ok(second_cancelled.load(Ordering::SeqCst)),
            )
            .unwrap_err()
            .to_string()
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let subscribers_dir = layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR);
        loop {
            let subscribed = match fs::read_dir(&subscribers_dir) {
                Ok(entries) => entries
                    .filter_map(std::result::Result::ok)
                    .any(|entry| entry.path().is_file()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => panic!("cannot inspect coordinator subscribers: {error}"),
            };
            if subscribed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cancelled follower did not subscribe to the preview attempt"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        cancelled.store(true, Ordering::SeqCst);
        assert!(
            second
                .join()
                .unwrap()
                .contains(BUILD_COORDINATOR_CANCELLED_ERROR)
        );
        assert_eq!(active_subscriber_count(&layout, &attempt_id).unwrap(), 1);

        allow_leader_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap().role, BuildCoordinatorRole::Leader);
        assert!(fs::read_dir(subscribers_dir).unwrap().next().is_none());
        assert_eq!(active_subscriber_count(&layout, &attempt_id).unwrap(), 0);
    }

    #[test]
    fn superseded_leader_publishes_failure_instead_of_success() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let superseded = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let build_superseded = superseded.clone();
        let error = coordinate_preview_build_with_verifier_and_cancel(
            &layout,
            key.key_hash(),
            || {
                publish_fake_artifact(&layout);
                build_superseded.store(true, Ordering::SeqCst);
                Ok(())
            },
            verify_completed_output,
            || Ok(superseded.load(Ordering::SeqCst)),
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
        let record = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap();
        assert_eq!(record.state, BuildCoordinatorState::Failed);
        assert_eq!(
            record.error.as_deref(),
            Some(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR)
        );
        assert_eq!(
            active_subscriber_count(&layout, &record.attempt_id).unwrap(),
            0
        );
    }

    #[test]
    fn cancelled_leader_without_followers_publishes_retryable_marker() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let build_cancelled = cancelled.clone();
        let error = coordinate_preview_build_with_verifier_and_caller_cancel(
            &layout,
            key.key_hash(),
            || {
                publish_fake_artifact(&layout);
                build_cancelled.store(true, Ordering::SeqCst);
                Ok(())
            },
            verify_completed_output,
            || Ok(cancelled.load(Ordering::SeqCst)),
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains(BUILD_COORDINATOR_CANCELLED_ERROR));
        let cancelled = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.state, BuildCoordinatorState::Cancelled);
        assert_eq!(
            cancelled.error.as_deref(),
            Some(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR)
        );

        let retry = coordinate_preview_build_with_verifier(
            &layout,
            key.key_hash(),
            || {
                publish_fake_artifact(&layout);
                Ok(())
            },
            verify_completed_output,
        )
        .unwrap();
        assert_eq!(retry.role, BuildCoordinatorRole::Leader);
        assert_ne!(retry.attempt_id, cancelled.attempt_id);
    }

    #[test]
    fn partial_attempt_is_not_a_cache_hit_and_can_be_rebuilt() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let attempts = Arc::new(AtomicUsize::new(0));
        let build_attempts = attempts.clone();
        let error = coordinate_preview_build_with_verifier(
            &layout,
            key.key_hash(),
            || {
                build_attempts.fetch_add(1, Ordering::SeqCst);
                // Even a verifiable file/manifest must not turn a partial
                // attempt into a reusable successful result.
                publish_fake_artifact(&layout);
                bail!(BUILD_COORDINATOR_PARTIAL_ERROR)
            },
            verify_completed_output,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains(BUILD_COORDINATOR_PARTIAL_ERROR));

        let partial = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap();
        assert_eq!(partial.state, BuildCoordinatorState::Partial);
        assert_eq!(
            partial.error.as_deref(),
            Some(BUILD_COORDINATOR_PARTIAL_ERROR)
        );

        let retry_attempts = attempts.clone();
        let retry = coordinate_preview_build_with_verifier(
            &layout,
            key.key_hash(),
            || {
                retry_attempts.fetch_add(1, Ordering::SeqCst);
                publish_fake_artifact(&layout);
                Ok(())
            },
            verify_completed_output,
        )
        .unwrap();
        assert_eq!(retry.role, BuildCoordinatorRole::Leader);
        assert_ne!(retry.attempt_id, partial.attempt_id);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
                .unwrap()
                .unwrap()
                .state,
            BuildCoordinatorState::Succeeded
        );
    }

    #[test]
    fn follower_retries_after_partial_attempt_instead_of_consuming_its_artifact() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let leader_layout = layout.clone();
        let leader_key_hash = key.key_hash().to_owned();
        let leader = std::thread::spawn(move || {
            coordinate_preview_build_with_verifier(
                &leader_layout,
                &leader_key_hash,
                || {
                    publish_fake_artifact(&leader_layout);
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    bail!(BUILD_COORDINATOR_PARTIAL_ERROR)
                },
                verify_completed_output,
            )
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let partial_attempt_id = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap()
            .attempt_id;

        let follower_layout = layout.clone();
        let follower_key_hash = key.key_hash().to_owned();
        let follower_build_count = Arc::new(AtomicUsize::new(0));
        let count = follower_build_count.clone();
        let follower = std::thread::spawn(move || {
            coordinate_preview_build_with_verifier(
                &follower_layout,
                &follower_key_hash,
                || {
                    count.fetch_add(1, Ordering::SeqCst);
                    publish_fake_artifact(&follower_layout);
                    Ok(())
                },
                verify_completed_output,
            )
            .unwrap()
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while active_subscriber_count(&layout, &partial_attempt_id).unwrap() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "follower did not subscribe to the partial attempt"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        release_tx.send(()).unwrap();

        let leader_error = leader.join().unwrap().unwrap_err();
        assert!(format!("{leader_error:#}").contains(BUILD_COORDINATOR_PARTIAL_ERROR));
        let follower_outcome = follower.join().unwrap();
        assert_eq!(follower_outcome.role, BuildCoordinatorRole::Leader);
        assert_ne!(follower_outcome.attempt_id, partial_attempt_id);
        assert_eq!(follower_build_count.load(Ordering::SeqCst), 1);
        let completed = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, BuildCoordinatorState::Succeeded);
        assert_eq!(completed.attempt_id, follower_outcome.attempt_id);
    }

    #[test]
    fn terminal_state_recognizes_partial_marker_through_context() {
        let error = anyhow::anyhow!(BUILD_COORDINATOR_PARTIAL_ERROR)
            .context("native build left an incomplete artifact set");
        assert_eq!(terminal_state(Some(&error)), BuildCoordinatorState::Partial);
        let compiler_error = anyhow::anyhow!("compiler failed");
        assert_eq!(
            terminal_state(Some(&compiler_error)),
            BuildCoordinatorState::Failed
        );
    }

    #[test]
    fn cancelled_leader_keeps_a_follower_owned_result_reusable() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let leader_layout = layout.clone();
        let leader_key_hash = key.key_hash().to_owned();
        let leader_cancelled = cancelled.clone();
        let leader = std::thread::spawn(move || {
            coordinate_preview_build_with_verifier_and_caller_cancel(
                &leader_layout,
                &leader_key_hash,
                || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    publish_fake_artifact(&leader_layout);
                    Ok(())
                },
                verify_completed_output,
                || Ok(leader_cancelled.load(Ordering::SeqCst)),
            )
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let follower_layout = layout.clone();
        let follower_key_hash = key.key_hash().to_owned();
        let (follower_started_tx, follower_started_rx) = std::sync::mpsc::channel();
        let follower = std::thread::spawn(move || {
            follower_started_tx.send(()).unwrap();
            coordinate_preview_build_with_verifier_and_cancel(
                &follower_layout,
                &follower_key_hash,
                || panic!("follower must not execute the shared build closure"),
                verify_completed_output,
                || Ok(false),
            )
            .unwrap()
        });
        follower_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();

        let attempt_id = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap()
            .attempt_id;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while active_subscriber_count(&layout, &attempt_id).unwrap() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "follower did not hold a shared coordinator reference"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        cancelled.store(true, Ordering::SeqCst);
        release_tx.send(()).unwrap();

        let leader_error = leader.join().unwrap().unwrap_err();
        assert!(format!("{leader_error:#}").contains(BUILD_COORDINATOR_CANCELLED_ERROR));
        let follower_outcome = follower.join().unwrap();
        assert_eq!(follower_outcome.role, BuildCoordinatorRole::Follower);
        assert_eq!(follower_outcome.attempt_id, attempt_id);
        let completed = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, BuildCoordinatorState::Succeeded);
        assert!(
            fs::read_dir(layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn controlled_leader_cancellation_terminates_only_without_followers() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let build_cancelled = cancelled.clone();
        let predicate_cancelled = cancelled.clone();
        let termination = terminated.clone();
        let error = coordinate_preview_build_with_leader_control(
            &layout,
            key.key_hash(),
            move |control| {
                build_cancelled.store(true, Ordering::SeqCst);
                assert_eq!(
                    control.should_abort().unwrap(),
                    Some(BuildCancellationReason::CallerCancelled)
                );
                let mut terminate = || {
                    termination.store(true, Ordering::SeqCst);
                    Ok(())
                };
                assert_eq!(
                    control.terminate_if_cancelled(&mut terminate).unwrap(),
                    Some(BuildCancellationReason::CallerCancelled)
                );
                Ok(())
            },
            verify_completed_output,
            move || {
                Ok(predicate_cancelled
                    .load(Ordering::SeqCst)
                    .then_some(BuildCoordinatorCancellationReason::CallerCancelled))
            },
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains(BUILD_COORDINATOR_CANCELLED_ERROR));
        assert!(terminated.load(Ordering::SeqCst));
        let record = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.error.as_deref(),
            Some(BUILD_COORDINATOR_LEADER_CANCELLED_ERROR)
        );
    }

    #[test]
    fn preview_attempt_does_not_join_an_ordinary_build_attempt() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let count = Arc::new(AtomicUsize::new(0));
        let first_layout = layout.clone();
        let first_key = key.clone();
        let first_count = count.clone();
        let first = std::thread::spawn(move || {
            coordinate_build(&first_layout, &first_key, || {
                first_count.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(150));
                publish_fake_artifact(&first_layout);
                Ok(())
            })
            .unwrap()
        });

        let state_path = layout.root.join(BUILD_COORDINATOR_STATE_FILE);
        for _ in 0..100 {
            if state_path.is_file() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state_path.is_file());

        let preview_layout = layout.clone();
        let preview_key = key.clone();
        let preview = coordinate_preview_build_with_verifier(
            &preview_layout,
            preview_key.key_hash(),
            || {
                count.fetch_add(1, Ordering::SeqCst);
                publish_fake_artifact(&preview_layout);
                Ok(())
            },
            verify_completed_output,
        )
        .unwrap();

        assert_eq!(first.join().unwrap().role, BuildCoordinatorRole::Leader);
        assert_eq!(preview.role, BuildCoordinatorRole::Leader);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(
            read_record(&layout, BUILD_COORDINATOR_BUILD_KIND)
                .unwrap()
                .unwrap()
                .kind,
            BUILD_COORDINATOR_BUILD_KIND
        );
        assert_eq!(
            read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
                .unwrap()
                .unwrap()
                .kind,
            BUILD_COORDINATOR_PREVIEW_KIND
        );
    }

    #[test]
    fn follower_retries_after_superseded_leader_is_cancelled() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let superseded = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let leader_layout = layout.clone();
        let leader_key_hash = key.key_hash().to_owned();
        let leader_superseded = superseded.clone();
        let leader = std::thread::spawn(move || {
            coordinate_preview_build_with_verifier_and_cancel(
                &leader_layout,
                &leader_key_hash,
                || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    publish_fake_artifact(&leader_layout);
                    leader_superseded.store(true, Ordering::SeqCst);
                    Ok(())
                },
                verify_completed_output,
                || Ok(leader_superseded.load(Ordering::SeqCst)),
            )
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let follower_layout = layout.clone();
        let follower_key_hash = key.key_hash().to_owned();
        let (follower_started_tx, follower_started_rx) = std::sync::mpsc::channel();
        let follower = std::thread::spawn(move || {
            follower_started_tx.send(()).unwrap();
            coordinate_preview_build_with_verifier_and_cancel(
                &follower_layout,
                &follower_key_hash,
                || {
                    publish_fake_artifact(&follower_layout);
                    Ok(())
                },
                verify_completed_output,
                || Ok(false),
            )
            .unwrap()
        });
        follower_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let attempt_id = loop {
            let subscribers = layout.root.join(BUILD_COORDINATOR_SUBSCRIBERS_DIR);
            let entries = fs::read_dir(subscribers)
                .map(|entries| entries.filter_map(std::result::Result::ok).count())
                .unwrap_or_default();
            if entries > 1 {
                let attempt_id = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
                    .unwrap()
                    .unwrap()
                    .attempt_id;
                break attempt_id;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "follower did not subscribe to the active preview attempt"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(active_subscriber_count(&layout, &attempt_id).unwrap(), 2);
        release_tx.send(()).unwrap();

        let leader_error = leader.join().unwrap().unwrap_err();
        assert!(format!("{leader_error:#}").contains(BUILD_COORDINATOR_LEADER_SUPERSEDED_ERROR));
        assert_eq!(follower.join().unwrap().role, BuildCoordinatorRole::Leader);
        let final_record = read_record(&layout, BUILD_COORDINATOR_PREVIEW_KIND)
            .unwrap()
            .unwrap();
        assert_eq!(final_record.state, BuildCoordinatorState::Succeeded);
        assert_ne!(final_record.attempt_id, attempt_id);
    }

    #[test]
    fn failed_attempt_is_shared_with_followers() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let (build_started_tx, build_started_rx) = std::sync::mpsc::channel();
        let (allow_failure_tx, allow_failure_rx) = std::sync::mpsc::channel();
        let first_layout = layout.clone();
        let first_key = key.clone();
        let first = std::thread::spawn(move || {
            coordinate_build(&first_layout, &first_key, || {
                build_started_tx.send(()).unwrap();
                allow_failure_rx.recv().unwrap();
                bail!("compiler failed")
            })
            .unwrap_err()
            .to_string()
        });

        build_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let second_layout = layout.clone();
        let second_key = key.clone();
        let (follower_started_tx, follower_started_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            follower_started_tx.send(()).unwrap();
            coordinate_build(&second_layout, &second_key, || {
                panic!("follower must not execute the failed build closure");
            })
            .unwrap_err()
            .to_string()
        });
        follower_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let attempt_id = read_record(&layout, BUILD_COORDINATOR_BUILD_KIND)
            .unwrap()
            .unwrap()
            .attempt_id;
        loop {
            if active_subscriber_count(&layout, &attempt_id).unwrap() > 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "follower did not subscribe to the active build attempt"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        allow_failure_tx.send(()).unwrap();

        assert!(first.join().unwrap().contains("compiler failed"));
        assert!(second.join().unwrap().contains("compiler failed"));
        let state_path = layout.root.join(BUILD_COORDINATOR_STATE_FILE);
        let state: BuildCoordinatorRecord =
            serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
        assert_eq!(state.state, BuildCoordinatorState::Failed);
        assert_eq!(state.error.as_deref(), Some("compiler failed"));
    }

    #[test]
    fn a_new_call_retries_after_a_failed_attempt_has_no_followers() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();

        assert!(coordinate_build(&layout, &key, || bail!("first failure")).is_err());
        let outcome = coordinate_build(&layout, &key, || {
            publish_fake_artifact(&layout);
            Ok(())
        })
        .unwrap();

        assert_eq!(outcome.role, BuildCoordinatorRole::Leader);
    }

    #[test]
    fn same_key_builders_share_across_processes() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        let mut child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "runner::build_coordinator::tests::coordinator_process_child",
                "--nocapture",
            ])
            .env("GPUI_COORDINATOR_TEST_ROOT", base.path())
            .spawn()
            .unwrap();

        let ready_path = base.path().join("coordinator-child-ready");
        let release_path = base.path().join("coordinator-child-release");
        let state_path = layout.root.join(BUILD_COORDINATOR_STATE_FILE);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready_path.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "coordinator child did not enter its build closure; state exists: {}",
                state_path.is_file()
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let follower_layout = layout.clone();
        let follower_key = key.clone();
        let follower = std::thread::spawn(move || {
            coordinate_build(&follower_layout, &follower_key, || {
                panic!("cross-process follower must not execute the build closure");
            })
        });
        let attempt_id = read_record(&layout, BUILD_COORDINATOR_BUILD_KIND)
            .unwrap()
            .unwrap()
            .attempt_id;
        while active_subscriber_count(&layout, &attempt_id).unwrap() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "cross-process follower did not register its subscriber"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        fs::write(&release_path, b"continue").unwrap();

        let outcome = follower.join().unwrap().unwrap();
        assert_eq!(outcome.role, BuildCoordinatorRole::Follower);
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn coordinator_process_child() {
        let Some(root) = env::var_os("GPUI_COORDINATOR_TEST_ROOT") else {
            return;
        };
        let base = PathBuf::from(root);
        let layout = layout(&base);
        let key = key();
        let outcome = coordinate_build(&layout, &key, || {
            let ready_path = base.join("coordinator-child-ready");
            let release_path = base.join("coordinator-child-release");
            fs::write(&ready_path, b"ready").unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !release_path.is_file() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "cross-process parent did not release the coordinator child"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            publish_fake_artifact(&layout);
            Ok(())
        })
        .unwrap();
        assert_eq!(outcome.role, BuildCoordinatorRole::Leader);
    }

    #[test]
    fn stale_building_record_can_be_replaced_after_owner_disappears() {
        let base = tempfile::tempdir().unwrap();
        let layout = layout(base.path());
        let key = key();
        fs::create_dir_all(&layout.root).unwrap();
        let stale = BuildCoordinatorRecord {
            schema_version: BUILD_COORDINATOR_SCHEMA_VERSION,
            kind: BUILD_COORDINATOR_BUILD_KIND.into(),
            platform: BuildPlatform::Desktop,
            key_hash: key.key_hash().into(),
            attempt_id: "stale-attempt".into(),
            owner_id: "stale-owner".into(),
            pid: 1,
            started_at_ms: 1,
            finished_at_ms: None,
            state: BuildCoordinatorState::Building,
            error: None,
        };
        write_record(&layout, &stale).unwrap();

        let outcome = coordinate_build(&layout, &key, || {
            publish_fake_artifact(&layout);
            Ok(())
        })
        .unwrap();
        assert_eq!(outcome.role, BuildCoordinatorRole::Leader);
        assert_ne!(outcome.attempt_id, "stale-attempt");
    }
}
