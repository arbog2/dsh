use chrono::Local;
use serde::Serialize;
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, State, WebviewBuilder,
    WebviewUrl, WindowEvent,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader as TokioBufReader},
    process::Command as AsyncCommand,
};

const TOP_BAR_HEIGHT: f64 = 64.0;
const DRAWER_HEIGHT: f64 = 300.0;
const UPDATE_LOG_RETENTION: usize = 30;
const COMMAND_OUTPUT_TAIL_LINES: usize = 60;
const MAX_UPDATE_LOG_BYTES: u64 = 32 * 1024 * 1024;
const MAX_LOG_LINE_CHARS: usize = 16_384;
const MAX_DESKTOP_LOG_BYTES: u64 = 8 * 1024 * 1024;
const PNPM_MIRROR_REGISTRY: &str = "https://registry.npmmirror.com/";
const NPM_OFFICIAL_REGISTRY: &str = "https://registry.npmjs.org/";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HarnessStatus {
    revision: u64,
    state: String,
    message: String,
    detail: String,
    port: Option<u16>,
    url: Option<String>,
    progress: u8,
    progress_label: String,
    update_in_progress: bool,
}

impl Default for HarnessStatus {
    fn default() -> Self {
        Self {
            revision: 0,
            state: "initializing".into(),
            message: "Checking the local runtime".into(),
            detail: "Verifying the bundled Harness files.".into(),
            port: None,
            url: None,
            progress: 0,
            progress_label: "Initialization".into(),
            update_in_progress: false,
        }
    }
}

#[derive(Default)]
struct AppState {
    status: Mutex<HarnessStatus>,
    child: Mutex<Option<tokio::process::Child>>,
    authenticated_url: Mutex<Option<String>>,
    diagnostics: Mutex<Option<DiagnosticLogState>>,
    generation: AtomicU64,
    update_in_progress: AtomicBool,
}

struct DiagnosticLogState {
    desktop: File,
    desktop_bytes: u64,
    desktop_truncated: bool,
    update: Option<UpdateLogState>,
}

struct UpdateLogState {
    file: File,
    path: PathBuf,
    started: Instant,
    bytes_written: u64,
    truncated: bool,
}

#[derive(Default)]
struct CommandOutput {
    lines: VecDeque<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogEvent {
    level: &'static str,
    line: String,
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> HarnessStatus {
    state.status.lock().expect("status lock poisoned").clone()
}

#[tauri::command]
async fn open_harness(app: AppHandle) -> Result<HarnessStatus, String> {
    let status = current_status(&app);
    let url = status
        .url
        .as_deref()
        .ok_or_else(|| "Harness is not running yet.".to_string())?;
    show_harness_view(&app, url)?;
    Ok(status)
}

#[tauri::command]
fn set_drawer_open(app: AppHandle, open: bool) -> Result<(), String> {
    set_harness_bounds(&app, open)
}

#[tauri::command]
async fn restart_harness(app: AppHandle) -> Result<HarnessStatus, String> {
    start_service(app).await
}

#[tauri::command]
async fn update_harness(
    app: AppHandle,
    state: State<'_, AppState>,
    force: Option<bool>,
) -> Result<HarnessStatus, String> {
    if state.update_in_progress.swap(true, Ordering::SeqCst) {
        return Err("An update is already in progress.".into());
    }

    if let Err(error) = begin_update_log(&app) {
        emit_log(
            &app,
            "warn",
            format!("Could not create the update diagnostic log: {error}"),
        );
    }
    let force = force.unwrap_or(false);
    if force {
        emit_log(
            &app,
            "info",
            "Forcing a full rebuild even if the checkout already matches origin/master.",
        );
    }
    let result = perform_update(app.clone(), force).await;
    finish_update_log(&app, result.as_ref());
    state.update_in_progress.store(false, Ordering::SeqCst);

    match result {
        Ok(mut status) => {
            // `perform_update` stops and restarts the service, and the status it
            // captured still carries the in-progress flag. Publishing that as-is
            // would leave every update control disabled until the next launch.
            status.update_in_progress = false;
            publish_status(&app, status.clone());
            Ok(status)
        }
        Err(error) => {
            set_error(&app, &error, "Update failed");
            Err(error)
        }
    }
}

/// Reads the Harness version from the runtime that is actually installed. The
/// packaged shell version is unrelated to the Harness release, so this has to be
/// resolved at runtime instead of being baked into the build.
fn harness_version(app: &AppHandle) -> Option<String> {
    let runtime_package = writable_runtime_dir(app)
        .ok()?
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("package.json");
    read_package_version(&runtime_package).or_else(|| {
        let source_package = writable_source_dir(app).ok()?.join("package.json");
        read_package_version(&source_package)
    })
}

fn read_package_version(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = manifest.get("version")?.as_str()?.trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

#[tauri::command]
fn get_harness_version(app: AppHandle) -> Result<String, String> {
    harness_version(&app).ok_or_else(|| "Harness runtime is not installed yet.".to_string())
}

#[tauri::command]
fn open_log_directory(app: AppHandle) -> Result<String, String> {
    let directory = log_directory(&app)?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("failed to create {}: {error}", directory.display()))?;

    #[cfg(target_os = "windows")]
    let result = Command::new("explorer.exe").arg(&directory).spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(&directory).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(&directory).spawn();

    result.map_err(|error| {
        format!(
            "failed to open log directory {}: {error}",
            directory.display()
        )
    })?;
    Ok(directory.to_string_lossy().to_string())
}

pub fn run() {
    let context = tauri::generate_context!();
    // Prune before `build`: Tauri creates the configured windows inside its own
    // setup step, and once WebView2 has the profile open the cookie database is
    // locked.
    let pruned_cookie_files = webview_profile_dir(&context.config().identifier)
        .map(|directory| prune_session_cookies(&directory))
        .unwrap_or_default();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            open_harness,
            restart_harness,
            update_harness,
            get_harness_version,
            open_log_directory,
            set_drawer_open
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            if let Err(error) = initialize_diagnostics(&handle) {
                eprintln!("failed to initialize diagnostic logging: {error}");
            }
            if !pruned_cookie_files.is_empty() {
                emit_log(
                    &handle,
                    "info",
                    format!(
                        "Cleared {} stored WebView2 session cookie file(s) before startup.",
                        pruned_cookie_files.len()
                    ),
                );
            }
            emit_log(
                &handle,
                "info",
                format!(
                    "DeepSeek Harness desktop {} starting on {}/{}",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            );
            tauri::async_runtime::spawn(async move {
                // The first launch clones and builds Harness from GitHub; every
                // later launch finds the installation in place and only has to
                // start the service.
                if let Err(error) = ensure_installation(&handle).await {
                    set_error(&handle, &error, "Initialization failed");
                    return;
                }
                if let Err(error) = start_service(handle.clone()).await {
                    set_error(&handle, &error, "Startup failed");
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                let app = window.app_handle().clone();
                stop_service(&app);
                app.exit(0);
            }
        })
        .build(context)
        .expect("failed to build DeepSeek Harness");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            stop_service(app_handle);
        }
    });
}

async fn start_service(app: AppHandle) -> Result<HarnessStatus, String> {
    stop_service(&app);
    *app.state::<AppState>()
        .authenticated_url
        .lock()
        .expect("authenticated URL lock poisoned") = None;
    set_phase(
        &app,
        "starting",
        "Starting Harness",
        "Launching the bundled Node.js service.",
        "Starting",
        10,
    );

    // The installation is guaranteed to be complete before the service starts,
    // so a damaged checkout is repaired by an update rather than here.
    let source_dir = writable_source_dir(&app)?;
    let node = bundled_node_path()?;

    let port = find_free_port()?;
    let url = format!("http://127.0.0.1:{port}");
    let runtime_entry = runtime_entry_path(&writable_runtime_dir(&app)?);
    if !runtime_entry.exists() {
        return Err(format!(
            "Harness runtime entry is missing at {}",
            runtime_entry.display()
        ));
    }

    let pnpm_home = resource_path(&app, "runtime/pnpm")?;
    let runtime_entry_arg = normalize_path_for_command(&runtime_entry);
    let source_dir_arg = normalize_path_for_command(&source_dir);
    let pnpm_home_arg = normalize_path_for_command(&pnpm_home);
    let mut command = AsyncCommand::new(&node);
    command
        .args([
            runtime_entry_arg,
            "web".into(),
            "--port".into(),
            port.to_string(),
            "--no-open".into(),
        ])
        .current_dir(Path::new(&source_dir_arg))
        .env("PNPM_HOME", pnpm_home_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // tokio's Command exposes the Windows creation flags directly, so no
        // `std::os::windows::process::CommandExt` import is needed here.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start Harness: {error}"))?;
    let pid = child
        .id()
        .ok_or_else(|| "the Harness process exited before it could be tracked".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture the Harness stdout stream".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture the Harness stderr stream".to_string())?;
    let generation = app.state::<AppState>().generation.fetch_add(1, Ordering::SeqCst) + 1;
    *app.state::<AppState>()
        .child
        .lock()
        .expect("child lock poisoned") = Some(child);

    emit_log(
        &app,
        "info",
        format!("Started Harness process {pid} on port {port}"),
    );
    // The stdout stream reaching EOF means the service is gone. That single
    // signal drives both the log and the unexpected-exit report, so no separate
    // process watcher is needed.
    let stdout_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut lines = TokioBufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = sanitize_log_line(line.trim_end());
            if let Some(authenticated_url) = extract_authenticated_url(&line, port) {
                *stdout_handle
                    .state::<AppState>()
                    .authenticated_url
                    .lock()
                    .expect("authenticated URL lock poisoned") =
                    Some(authenticated_url);
            }
            emit_log(&stdout_handle, "info", line);
        }
        let state = stdout_handle.state::<AppState>();
        if state.generation.load(Ordering::SeqCst) != generation {
            // A restart or update stopped this process on purpose.
            return;
        }
        state.child.lock().expect("child lock poisoned").take();
        set_error(
            &stdout_handle,
            "Harness stopped unexpectedly.",
            "Harness stopped",
        );
    });
    let stderr_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut lines = TokioBufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            emit_log(
                &stderr_handle,
                "warn",
                sanitize_log_line(line.trim_end()),
            );
        }
    });

    set_phase(
        &app,
        "starting",
        "Waiting for the Web service",
        "The Harness window opens when the local endpoint is ready.",
        "Health check",
        45,
    );
    let authenticated_url = wait_for_health(&app, &url).await?;

    let status = HarnessStatus {
        revision: 0,
        state: "running".into(),
        message: "Harness is running".into(),
        detail: "The local Web service is ready.".into(),
        port: Some(port),
        url: Some(authenticated_url.clone()),
        progress: 100,
        progress_label: "Ready".into(),
        update_in_progress: app.state::<AppState>().update_in_progress.load(Ordering::SeqCst),
    };
    show_harness_view(&app, &authenticated_url)?;
    publish_status(&app, status.clone());
    emit_log(
        &app,
        "info",
        format!("Harness is ready at {authenticated_url}"),
    );
    Ok(status)
}

/// Where the writable checkout is cloned from on first launch and on repair.
const HARNESS_CLONE_URL: &str = "https://github.com/deepseek-ai/deepseek-harness.git";
/// The branch the desktop shell tracks. Updates fetch this ref directly.
const HARNESS_BRANCH: &str = "master";

/// Result of the checkout comparison that opens an update.
enum UpdateOutcome {
    /// The writable checkout already points at the fetched revision.
    UpToDate { revision: String },
    /// A fresh runtime was built and activated.
    Rebuilt,
}

/// Git prints full 40 character object names; the UI only needs enough to tell
/// two revisions apart.
fn short_revision(revision: &str) -> String {
    revision.chars().take(7).collect()
}

/// A rebuild is only skipped when the fetched revision is known to match the
/// checkout. An unreadable local revision (`""`) is treated as unknown and
/// falls through to a full rebuild, which is the safe direction.
fn update_needs_rebuild(local_revision: &str, remote_revision: &str, force: bool) -> bool {
    if force {
        return true;
    }
    local_revision.is_empty() || remote_revision.is_empty() || local_revision != remote_revision
}

/// A source checkout is usable when it is a Git work tree with a manifest. The
/// desktop shell rebuilds from it, so `.git` is as important as the sources.
fn source_is_installed(directory: &Path) -> bool {
    directory.join(".git").is_dir() && directory.join("package.json").is_file()
}

/// A runtime is usable when the deployed dependency tree exposes the CLI entry
/// point. The package manifest is checked too because the deploy step rewrites
/// it and a half-written tree must not be mistaken for a working one.
fn runtime_is_installed(directory: &Path) -> bool {
    runtime_entry_path(directory).is_file() && directory.join("package.json").is_file()
}

/// True when both halves of the installation are present.
fn installation_is_complete(app: &AppHandle) -> Result<bool, String> {
    Ok(source_is_installed(&writable_source_dir(app)?)
        && runtime_is_installed(&writable_runtime_dir(app)?))
}

/// Clones the tracked branch when the writable checkout is missing or damaged.
///
/// The packaged application deliberately does not ship a source archive: the
/// download is a few dozen megabytes and guarantees the first run starts from
/// the newest revision instead of whatever was current when the executable was
/// built.
async fn ensure_source_checkout(app: &AppHandle, source_dir: &Path) -> Result<(), String> {
    if source_is_installed(source_dir) {
        return Ok(());
    }
    let git = resource_path(app, "runtime/git/cmd/git.exe")?;
    remove_directory_if_exists(source_dir)?;
    if let Some(parent) = source_dir.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    set_install_progress(
        app,
        15,
        "Cloning the Harness source repository",
        "Fetching the tracked branch from GitHub.",
    );
    let target = normalize_path_for_command(source_dir);
    run_command(
        app,
        &git,
        vec![
            "clone".into(),
            "--depth".into(),
            "1".into(),
            "--branch".into(),
            HARNESS_BRANCH.into(),
            "--single-branch".into(),
            HARNESS_CLONE_URL.into(),
            target,
        ],
        source_dir
            .parent()
            .ok_or_else(|| format!("invalid source path {}", source_dir.display()))?,
        Vec::new(),
        "git clone",
    )
    .await?;
    if !source_is_installed(source_dir) {
        return Err(format!(
            "the cloned checkout is missing its Git metadata at {}",
            source_dir.display()
        ));
    }
    Ok(())
}

/// Installs dependencies, builds the workspace, and deploys a production-only
/// dependency tree into `next_runtime`.
///
/// This is the whole build half of a first run and of an update. It never
/// touches the active runtime, so a failure leaves the previous installation
/// exactly as it was.
async fn build_runtime_into(
    app: &AppHandle,
    source_dir: &Path,
    next_runtime: &Path,
    installing: bool,
) -> Result<(), String> {
    let node = bundled_node_path()?;
    let pnpm = resource_path(app, "runtime/pnpm/pnpm.cjs")?;
    let pnpm_home = resource_path(app, "runtime/pnpm")?;

    report_build_progress(
        app,
        installing,
        24,
        "Installing dependencies",
        "Resolving the locked Harness workspace.",
    );
    run_pnpm_with_registry_fallback(
        app,
        &node,
        &pnpm,
        source_dir,
        &pnpm_home,
        strings(["install", "--frozen-lockfile"]),
        "pnpm install",
    )
    .await?;

    report_build_progress(
        app,
        installing,
        52,
        "Cleaning build state",
        "Removing stale incremental build artifacts.",
    );
    run_pnpm(
        app,
        &node,
        &pnpm,
        source_dir,
        &pnpm_home,
        strings(["run", "clean"]),
        "pnpm run clean",
    )
    .await?;

    report_build_progress(
        app,
        installing,
        61,
        "Building Harness",
        "Compiling the host, client, and Web application.",
    );
    run_pnpm(
        app,
        &node,
        &pnpm,
        source_dir,
        &pnpm_home,
        strings(["run", "build"]),
        "pnpm run build",
    )
    .await?;

    report_build_progress(
        app,
        installing,
        88,
        "Packaging runtime",
        "Creating a production-only dependency tree.",
    );
    remove_directory_if_exists(next_runtime)?;
    run_pnpm_with_registry_fallback(
        app,
        &node,
        &pnpm,
        source_dir,
        &pnpm_home,
        vec![
            "--filter".into(),
            "dsh-python-runtime-closure".into(),
            "deploy".into(),
            "--legacy".into(),
            "--prod".into(),
            "--config.allow-unused-patches=true".into(),
            "--config.node-linker=hoisted".into(),
            "--config.auto-install-peers=true".into(),
            "--config.link-workspace-packages=true".into(),
            "--config.ignore-scripts=true".into(),
            normalize_path_for_command(next_runtime),
        ],
        "pnpm deploy",
    )
    .await?;

    let repair_script = resource_path(app, "tools/repair-runtime.mjs")?;
    run_command(
        app,
        &node,
        vec![
            normalize_path_for_command(&repair_script),
            normalize_path_for_command(source_dir),
            normalize_path_for_command(next_runtime),
        ],
        source_dir,
        Vec::new(),
        "runtime dependency repair",
    )
    .await?;
    prepare_deployed_runtime(app, &node, next_runtime).await?;
    remove_source_node_modules(source_dir)?;

    if !runtime_is_installed(next_runtime) {
        return Err(format!(
            "New runtime entry is missing at {}",
            runtime_entry_path(next_runtime).display()
        ));
    }
    Ok(())
}

/// Builds the very first installation. Runs once per machine: every later
/// launch finds both directories in place and only has to start the service.
async fn ensure_installation(app: &AppHandle) -> Result<(), String> {
    if installation_is_complete(app)? {
        emit_log(
            app,
            "info",
            "Harness is already installed; skipping the first-run download.",
        );
        return Ok(());
    }

    set_phase(
        app,
        "initializing",
        "Preparing the first-run environment",
        "Downloading the latest Harness source from GitHub. This only happens once.",
        "Initialization",
        5,
    );

    let source_dir = writable_source_dir(app)?;
    let active_runtime = writable_runtime_dir(app)?;
    let next_runtime = active_runtime.with_file_name("runtime-next");
    let backup_runtime = active_runtime.with_file_name("runtime-backup");

    // A half-finished earlier attempt must not be mistaken for a usable
    // installation, and a damaged checkout is simply replaced by a fresh clone.
    ensure_source_checkout(app, &source_dir).await?;
    let build_result = build_runtime_into(app, &source_dir, &next_runtime, true).await;
    if let Err(error) = build_result {
        let _ = remove_directory_if_exists(&next_runtime);
        let _ = remove_directory_if_exists(&backup_runtime);
        return Err(format!(
            "The first-run build failed: {error}. Check the network connection and press Update to try again."
        ));
    }

    set_install_progress(
        app,
        96,
        "Activating runtime",
        "Switching to the newly built Harness release.",
    );
    if let Err(error) = activate_runtime(&active_runtime, &next_runtime, &backup_runtime) {
        let _ = remove_directory_if_exists(&next_runtime);
        return Err(format!("Failed to activate the first-run runtime: {error}"));
    }
    let _ = remove_directory_if_exists(&backup_runtime);
    emit_log(
        app,
        "info",
        "First-run installation completed; starting the Harness service.",
    );
    Ok(())
}

async fn perform_update(app: AppHandle, force: bool) -> Result<HarnessStatus, String> {
    let source_dir = writable_source_dir(&app)?;
    let active_runtime = writable_runtime_dir(&app)?;
    let next_runtime = active_runtime.with_file_name("runtime-next");
    let backup_runtime = active_runtime.with_file_name("runtime-backup");

    stop_service(&app);
    set_phase(
        &app,
        "updating",
        "Updating Harness",
        "Fetching the latest source revision.",
        "Git fetch",
        5,
    );

    let git = resource_path(&app, "runtime/git/cmd/git.exe")?;
    // An update is also the repair path: a missing or damaged checkout is
    // cloned again instead of failing.
    if let Err(error) = ensure_source_checkout(&app, &source_dir).await {
        return recover_after_update_failure(app, error).await;
    }

    let build_result = async {
        let local_revision = run_command(
            &app,
            &git,
            strings(["rev-parse", "HEAD"]),
            &source_dir,
            Vec::new(),
            "git rev-parse HEAD",
        )
        .await?;
        run_command(
            &app,
            &git,
            strings(["fetch", "--prune", "--depth", "1", "origin", HARNESS_BRANCH]),
            &source_dir,
            Vec::new(),
            "git fetch",
        )
        .await?;
        let remote_revision = run_command(
            &app,
            &git,
            strings(["rev-parse", "FETCH_HEAD"]),
            &source_dir,
            Vec::new(),
            "git rev-parse FETCH_HEAD",
        )
        .await?;

        if !update_needs_rebuild(&local_revision, &remote_revision, force) {
            // Rebuilding an identical revision costs minutes and produces the
            // same runtime, so the update stops here. `force` is the escape
            // hatch for a runtime that was damaged locally.
            return Ok(UpdateOutcome::UpToDate {
                revision: short_revision(&remote_revision),
            });
        }

        set_update_progress(&app, 14, "Applying source", "Updating the writable checkout.");
        run_command(
            &app,
            &git,
            strings(["reset", "--hard", "FETCH_HEAD"]),
            &source_dir,
            Vec::new(),
            "git reset",
        )
        .await?;
        run_command(
            &app,
            &git,
            strings(["clean", "-fd", "-e", "node_modules", "-e", ".pnpm-store"]),
            &source_dir,
            Vec::new(),
            "git clean",
        )
        .await?;

        build_runtime_into(&app, &source_dir, &next_runtime, false).await?;
        Ok(UpdateOutcome::Rebuilt)
    }
    .await;

    let outcome = match build_result {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = remove_directory_if_exists(&next_runtime);
            return recover_after_update_failure(app, error).await;
        }
    };

    if let UpdateOutcome::UpToDate { revision } = outcome {
        // The service was stopped before the version check, so it has to come
        // back up; nothing else changed.
        let mut status = match start_service(app.clone()).await {
            Ok(status) => status,
            Err(error) => {
                return Err(format!(
                    "The checkout already matches origin/master ({revision}) but the Harness service failed to restart: {error}"
                ))
            }
        };
        status.message = "Harness is up to date".into();
        status.detail = format!("The checkout already matches origin/master ({revision}).");
        status.progress = 100;
        status.progress_label = "Up to date".into();
        publish_status(&app, status.clone());
        emit_log(
            &app,
            "info",
            format!("Harness is already at the latest revision ({revision}); nothing to rebuild."),
        );
        return Ok(status);
    }

    set_update_progress(
        &app,
        96,
        "Activating runtime",
        "Switching to the newly built Harness release.",
    );
    if let Err(error) = activate_runtime(&active_runtime, &next_runtime, &backup_runtime) {
        let _ = remove_directory_if_exists(&next_runtime);
        return recover_after_update_failure(app, error).await;
    }

    match start_service(app.clone()).await {
        Ok(mut status) => {
            let _ = remove_directory_if_exists(&backup_runtime);
            status.message = "Harness updated".into();
            status.detail = "The latest revision is running.".into();
            status.progress = 100;
            status.progress_label = "Updated".into();
            publish_status(&app, status.clone());
            emit_log(&app, "info", "Harness update completed successfully.");
            Ok(status)
        }
        Err(start_error) => {
            stop_service(&app);
            if let Err(rollback_error) = restore_previous_runtime(&active_runtime, &backup_runtime) {
                return Err(format!(
                    "The updated runtime failed to start: {start_error}. Rollback also failed: {rollback_error}"
                ));
            }
            match start_service(app.clone()).await {
                Ok(mut status) => {
                    let detail =
                        format!("Update failed and the previous version was restored: {start_error}");
                    status.message = "Previous version restored".into();
                    status.detail = detail.clone();
                    status.progress = 100;
                    status.progress_label = "Update failed".into();
                    publish_status(&app, status.clone());
                    emit_log(&app, "error", detail);
                    Ok(status)
                }
                Err(rollback_start_error) => Err(format!(
                    "The updated runtime failed to start: {start_error}. The previous runtime was restored but also failed to start: {rollback_start_error}"
                )),
            }
        }
    }
}
async fn prepare_deployed_runtime(
    app: &AppHandle,
    node: &Path,
    runtime_dir: &Path,
) -> Result<(), String> {
    let koffi = runtime_dir.join("node_modules").join("koffi");
    if koffi.join("cnoke.cjs").exists() {
        run_command(
            app,
            node,
            vec![
                "cnoke.cjs".into(),
                "-P".into(),
                normalize_path_for_command(&koffi),
                "-D".into(),
                "src/koffi".into(),
                "--prebuild".into(),
                "--release".into(),
            ],
            &koffi,
            Vec::new(),
            "koffi native runtime",
        )
        .await?;
    }

    let node_pty = runtime_dir.join("node_modules").join("node-pty");
    if node_pty.join("scripts").join("post-install.js").exists() {
        run_command(
            app,
            node,
            vec!["scripts/post-install.js".into()],
            &node_pty,
            Vec::new(),
            "node-pty runtime",
        )
        .await?;
    }

    let subprocess = runtime_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh-subprocess-local");
    if subprocess
        .join("scripts")
        .join("ensure-spawn-helper.mjs")
        .exists()
    {
        run_command(
            app,
            node,
            vec!["scripts/ensure-spawn-helper.mjs".into()],
            &subprocess,
            Vec::new(),
            "subprocess helper",
        )
        .await?;
    }
    Ok(())
}

async fn recover_after_update_failure(
    app: AppHandle,
    error: String,
) -> Result<HarnessStatus, String> {
    emit_log(&app, "error", format!("Update failed: {error}"));
    match start_service(app.clone()).await {
        Ok(mut status) => {
            status.message = "Update failed; current runtime kept".into();
            status.detail = error;
            status.progress = 100;
            status.progress_label = "Update failed".into();
            publish_status(&app, status.clone());
            Ok(status)
        }
        Err(start_error) => Err(format!(
            "Update failed: {error}. The previous runtime also failed to start: {start_error}"
        )),
    }
}

/// Path of the Node.js runtime that executes the Harness CLI, the package
/// manager, and the helper scripts. The sidecar copy installed next to the
/// executable is the only one the bundle ships.
fn bundled_node_path() -> Result<PathBuf, String> {
    let mut executable = std::env::current_exe()
        .map_err(|error| format!("failed to resolve the running executable: {error}"))?;
    executable.set_file_name("node.exe");
    if !executable.is_file() {
        return Err(format!(
            "the bundled Node.js runtime is missing at {}",
            executable.display()
        ));
    }
    Ok(executable)
}

fn runtime_entry_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js")
}
async fn run_pnpm(
    app: &AppHandle,
    node: &Path,
    pnpm: &Path,
    cwd: &Path,
    pnpm_home: &Path,
    args: Vec<String>,
    label: &str,
) -> Result<(), String> {
    run_command(
        app,
        node,
        pnpm_command_args(pnpm, args),
        cwd,
        vec![
            (
                "PNPM_HOME".into(),
                normalize_path_for_command(pnpm_home),
            ),
            ("npm_config_registry".into(), PNPM_MIRROR_REGISTRY.into()),
            (
                "NPM_CONFIG_REGISTRY".into(),
                PNPM_MIRROR_REGISTRY.into(),
            ),
        ],
        label,
    )
    .await
    .map(|_| ())
}

async fn run_pnpm_with_registry_fallback(
    app: &AppHandle,
    node: &Path,
    pnpm: &Path,
    cwd: &Path,
    pnpm_home: &Path,
    args: Vec<String>,
    label: &str,
) -> Result<(), String> {
    match run_pnpm_with_registry(
        app,
        node,
        pnpm,
        cwd,
        pnpm_home,
        PNPM_MIRROR_REGISTRY,
        args.clone(),
        label,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(mirror_error) => {
            emit_log(
                app,
                "warn",
                format!(
                    "{label} failed through the preferred npm mirror; retrying with the official registry: {mirror_error}"
                ),
            );
            run_pnpm_with_registry(
                app,
                node,
                pnpm,
                cwd,
                pnpm_home,
                NPM_OFFICIAL_REGISTRY,
                args,
                label,
            )
            .await
            .map_err(|official_error| {
                format!(
                    "{label} failed through both npm registries.\n  mirror error: {mirror_error}\n  official error: {official_error}"
                )
            })
        }
    }
}

async fn run_pnpm_with_registry(
    app: &AppHandle,
    node: &Path,
    pnpm: &Path,
    cwd: &Path,
    pnpm_home: &Path,
    registry: &str,
    args: Vec<String>,
    label: &str,
) -> Result<(), String> {
    run_command(
        app,
        node,
        pnpm_registry_command_args(pnpm, registry, args),
        cwd,
        vec![
            (
                "PNPM_HOME".into(),
                normalize_path_for_command(pnpm_home),
            ),
            ("npm_config_registry".into(), registry.to_string()),
            ("NPM_CONFIG_REGISTRY".into(), registry.to_string()),
            ("npm_config_fetch_retries".into(), "3".into()),
            ("npm_config_fetch_timeout".into(), "120000".into()),
        ],
        label,
    )
    .await
    .map(|_| ())
}

fn pnpm_command_args(pnpm: &Path, args: Vec<String>) -> Vec<String> {
    let mut command_args = Vec::with_capacity(args.len() + 3);
    command_args.push(normalize_path_for_command(pnpm));
    // pnpm only consumes config flags before the subcommand. Putting these
    // after `run` forwards them to the package script instead.
    command_args.push("--config.confirmModulesPurge=false".into());
    command_args.push("--config.verify-deps-before-run=false".into());
    command_args.extend(args);
    command_args
}

fn pnpm_registry_command_args(
    pnpm: &Path,
    registry: &str,
    args: Vec<String>,
) -> Vec<String> {
    let mut command_args = pnpm_command_args(pnpm, args);
    command_args.insert(3, format!("--registry={registry}"));
    command_args
}

/// Runs a helper program to completion and returns its captured stdout (ANSI
/// escapes stripped, trailing whitespace trimmed). Callers that only care about
/// success can ignore the value.
async fn run_command(
    app: &AppHandle,
    program: &Path,
    args: Vec<String>,
    cwd: &Path,
    environment: Vec<(OsString, String)>,
    label: &str,
) -> Result<String, String> {
    // Tauri resource paths are verbatim on Windows (`\\?\D:\...`). pnpm
    // miscomputes lifecycle paths such as npm_execpath for that form.
    let program = normalize_windows_path(program);
    let cwd = normalize_windows_path(cwd);
    let command_line = format!(
        "{} {}",
        program.display(),
        args.iter()
            .map(|argument| quote_argument(argument))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let environment_text = environment
        .iter()
        .map(|(key, value)| format!("{}={}", key.to_string_lossy(), quote_argument(value)))
        .collect::<Vec<_>>()
        .join(", ");
    emit_log(
        app,
        "info",
        format!(
            "Starting {label}: {command_line}\n  cwd: {}\n  env: {}",
            cwd.display(),
            if environment_text.is_empty() {
                "(inherited)"
            } else {
                &environment_text
            }
        ),
    );

    let mut command = AsyncCommand::new(&program);
    command
        .args(args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CI", "1")
        // pnpm, tsdown, rolldown, and git all suppress color when NO_COLOR is
        // set; without it they emit ANSI escapes that end up in the log files
        // and the status drawer, where nothing renders them.
        .env("NO_COLOR", "1")
        .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0");
    for (key, value) in environment {
        command.env(key, value);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = format!("failed to start {label}: {error}");
            write_update_log(app, "error", format!("{message}\n  command: {command_line}"));
            return Err(message);
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| {
            let message = format!("failed to capture stdout for {label}");
            write_update_log(app, "error", &message);
            message
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| {
            let message = format!("failed to capture stderr for {label}");
            write_update_log(app, "error", &message);
            message
        })?;

    let stdout_app = app.clone();
    let stdout_output = Arc::new(Mutex::new(CommandOutput::default()));
    let stdout_output_task = stdout_output.clone();
    let stdout_task = tauri::async_runtime::spawn(async move {
        let mut lines = TokioBufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            for frame in progress_frames(&line) {
                let frame = sanitize_log_line(frame);
                push_command_output(&stdout_output_task, &frame);
                emit_log(&stdout_app, "info", frame);
            }
        }
    });
    let stderr_app = app.clone();
    let stderr_output = Arc::new(Mutex::new(CommandOutput::default()));
    let stderr_output_task = stderr_output.clone();
    let stderr_task = tauri::async_runtime::spawn(async move {
        let mut lines = TokioBufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            for frame in progress_frames(&line) {
                let frame = sanitize_log_line(frame);
                push_command_output(&stderr_output_task, &frame);
                emit_log(&stderr_app, "warn", frame);
            }
        }
    });

    let status = match child.wait().await {
        Ok(status) => status,
        Err(error) => {
            let message = format!("failed while waiting for {label}: {error}");
            write_update_log(app, "error", &message);
            return Err(message);
        }
    };
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    if status.success() {
        let stdout_tail = command_output_text(&stdout_output);
        write_update_log(
            app,
            "info",
            format!(
                "Finished {label} successfully\n  command: {command_line}\n  exitCode: {}\n  stdout tail:\n{}",
                status.code().map_or("unknown".into(), |code| code.to_string()),
                indent_log_block(&stdout_tail),
            ),
        );
        Ok(stdout_tail.trim().to_string())
    } else {
        let message = format!(
            "{label} failed with {}",
            status
                .code()
                .map_or_else(|| "an unknown status".to_string(), |code| format!("exit code {code}"))
        );
        let stdout_tail = command_output_text(&stdout_output);
        let stderr_tail = command_output_text(&stderr_output);
        write_update_log(
            app,
            "error",
            format!(
                "{message}\n  command: {command_line}\n  cwd: {}\n  exitCode: {}\n  stdout tail:\n{}\n  stderr tail:\n{}",
                cwd.display(),
                status.code().map_or("unknown".into(), |code| code.to_string()),
                indent_log_block(&stdout_tail),
                indent_log_block(&stderr_tail),
            ),
        );
        Err(message)
    }
}

async fn wait_for_health(app: &AppHandle, base_url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("failed to create health client: {error}"))?;

    for _ in 0..180 {
        if app
            .state::<AppState>()
            .child
            .lock()
            .expect("child lock poisoned")
            .is_none()
        {
            return Err("Harness exited before becoming ready.".into());
        }
        let authenticated_url = app
            .state::<AppState>()
            .authenticated_url
            .lock()
            .expect("authenticated URL lock poisoned")
            .clone();
        let Some(authenticated_url) = authenticated_url else {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        if let Ok(response) = client.get(&authenticated_url).send().await {
            if response.status().as_u16() < 400 {
                return Ok(authenticated_url);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    stop_service(app);
    Err(format!(
        "Timed out waiting for the Harness Web service at {base_url}."
    ))
}

fn show_harness_view(app: &AppHandle, url: &str) -> Result<(), String> {
    let parsed = url
        .parse()
        .map_err(|error| format!("invalid Harness URL {url}: {error}"))?;
    if let Some(webview) = app.get_webview("harness") {
        webview
            .navigate(parsed)
            .map_err(|error| format!("failed to navigate Harness view: {error}"))?;
        set_harness_bounds(app, false)?;
        let _ = webview.set_focus();
        return Ok(());
    }

    let window = app
        .get_window("main")
        .ok_or_else(|| "Main control window is missing.".to_string())?;
    let scale_factor = window
        .scale_factor()
        .map_err(|error| format!("failed to read window scale: {error}"))?;
    let window_size = window
        .inner_size()
        .map_err(|error| format!("failed to read window size: {error}"))?;
    let top = logical_to_physical(TOP_BAR_HEIGHT, scale_factor);
    let height = window_size.height.saturating_sub(top).max(1);
    window
        .add_child(
            WebviewBuilder::new("harness", WebviewUrl::External(parsed))
                .auto_resize()
                .focused(true),
            PhysicalPosition::new(0, top),
            PhysicalSize::new(window_size.width, height),
        )
        .map_err(|error| format!("failed to create Harness view: {error}"))?;
    set_harness_bounds(app, false)?;
    Ok(())
}

fn set_harness_bounds(app: &AppHandle, drawer_open: bool) -> Result<(), String> {
    let Some(harness) = app.get_webview("harness") else {
        return Ok(());
    };
    let window = app
        .get_window("main")
        .ok_or_else(|| "Main control window is missing.".to_string())?;
    let scale_factor = window
        .scale_factor()
        .map_err(|error| format!("failed to read window scale: {error}"))?;
    let window_size = window
        .inner_size()
        .map_err(|error| format!("failed to read window size: {error}"))?;
    let logical_top = TOP_BAR_HEIGHT + if drawer_open { DRAWER_HEIGHT } else { 0.0 };
    let top = logical_to_physical(logical_top, scale_factor);
    let height = window_size.height.saturating_sub(top).max(1);
    harness
        .set_position(PhysicalPosition::new(0, top))
        .map_err(|error| format!("failed to position Harness view: {error}"))?;
    harness
        .set_size(PhysicalSize::new(window_size.width, height))
        .map_err(|error| format!("failed to resize Harness view: {error}"))?;
    Ok(())
}

fn logical_to_physical(value: f64, scale_factor: f64) -> u32 {
    (value * scale_factor).round().max(0.0) as u32
}

fn stop_service(app: &AppHandle) {
    let generation = app
        .state::<AppState>()
        .generation
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    let child = app
        .state::<AppState>()
        .child
        .lock()
        .expect("child lock poisoned")
        .take();
    *app.state::<AppState>()
        .authenticated_url
        .lock()
        .expect("authenticated URL lock poisoned") = None;

    if let Some(child) = child {
        if let Some(pid) = child.id() {
            emit_log(app, "info", format!("Stopping Harness process {pid}"));
            kill_process_tree(pid);
        }
        drop(child);
        if generation > 0 {
            let state = app.state::<AppState>();
            let mut status = state.status.lock().expect("status lock poisoned");
            if status.state == "running" || status.state == "starting" {
                status.state = "stopping".into();
                status.message = "Stopping Harness".into();
                status.detail = "Closing the local service process tree.".into();
                status.progress_label = "Stopping".into();
            }
        }
    }
}

fn extract_authenticated_url(line: &str, port: u16) -> Option<String> {
    for candidate in line.split_whitespace() {
        let candidate = candidate.trim_matches(|character: char| {
            matches!(character, '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';')
        });
        let Ok(url) = url::Url::parse(candidate) else {
            continue;
        };
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port() != Some(port)
            || url.path() != "/"
        {
            continue;
        }
        if url
            .query_pairs()
            .any(|(key, value)| key == "token" && !value.is_empty())
        {
            return Some(url.to_string());
        }
    }
    None
}

/// Terminates a service process and everything it spawned.
///
/// The service is started directly through `std::process::Command`, so there is
/// no sidecar handle to kill; the tree is torn down by PID instead.
fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

fn activate_runtime(
    active_runtime: &Path,
    next_runtime: &Path,
    backup_runtime: &Path,
) -> Result<(), String> {
    remove_directory_if_exists(backup_runtime)?;
    if active_runtime.exists() {
        fs::rename(active_runtime, backup_runtime).map_err(|error| {
            format!(
                "failed to back up {} to {}: {error}",
                active_runtime.display(),
                backup_runtime.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(next_runtime, active_runtime) {
        let _ = fs::rename(backup_runtime, active_runtime);
        return Err(format!(
            "failed to activate {} at {}: {error}",
            next_runtime.display(),
            active_runtime.display()
        ));
    }
    Ok(())
}

fn restore_previous_runtime(active_runtime: &Path, backup_runtime: &Path) -> Result<(), String> {
    remove_directory_if_exists(active_runtime)?;
    fs::rename(backup_runtime, active_runtime).map_err(|error| {
        format!(
            "failed to restore {} to {}: {error}",
            backup_runtime.display(),
            active_runtime.display()
        )
    })
}

fn remove_directory_if_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
    }
    Ok(())
}

fn remove_source_node_modules(directory: &Path) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to read source entry: {error}"))?;
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?
            .is_dir()
        {
            continue;
        }
        if name == "node_modules" {
            fs::remove_dir_all(&path)
                .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
        } else {
            remove_source_node_modules(&path)?;
        }
    }
    Ok(())
}

/// Directory holding the WebView2 profile (cookies, cache, local storage).
fn webview_profile_dir(identifier: &str) -> Option<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local_app_data).join(identifier).join("EBWebView"))
}

/// Drop the persisted session cookies before the WebView2 profile is opened.
///
/// Harness authenticates the browser session with an authority-scoped cookie
/// (`dsh-auth-<hash of host and port>`), and the shell serves Harness on a fresh
/// loopback port every launch. Cookies are scoped by host only, so each launch
/// leaves another `dsh-auth-*` cookie behind and the browser then sends every
/// one of them. Once the accumulated `Cookie` header passes the request-header
/// limit, the plugin bundle request fails and the boot page stops at "Failed to
/// load plugins".
///
/// The shell always re-authenticates through the token URL it prints, so the
/// stored cookies carry nothing worth keeping. Removing them keeps the header
/// small and the boot deterministic.
///
/// @param profile_dir - the WebView2 profile directory.
/// @returns the paths that were removed, for diagnostic logging.
fn prune_session_cookies(profile_dir: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for relative in [
        "Default/Network/Cookies",
        "Default/Network/Cookies-journal",
        "Default/Cookies",
        "Default/Cookies-journal",
    ] {
        let path = profile_dir.join(relative);
        match fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            // Absent on a first launch, and a lingering WebView2 process can
            // hold the file; neither may block startup.
            Err(_) => {}
        }
    }
    removed
}

fn find_free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("failed to allocate a local port: {error}"))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| format!("failed to read the allocated local port: {error}"))
}

fn resource_path(app: &AppHandle, relative: &str) -> Result<PathBuf, String> {
    app.path()
        .resource_dir()
        .map(|directory| directory.join(relative))
        .map_err(|error| format!("failed to resolve resource directory: {error}"))
}

fn writable_harness_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("harness"))
        .map_err(|error| format!("failed to resolve app data directory: {error}"))
}

fn writable_source_dir(app: &AppHandle) -> Result<PathBuf, String> {
    writable_harness_root(app).map(|directory| directory.join("source"))
}

fn writable_runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    writable_harness_root(app).map(|directory| directory.join("runtime"))
}

fn log_directory(app: &AppHandle) -> Result<PathBuf, String> {
    writable_harness_root(app).map(|directory| directory.join("logs"))
}

fn initialize_diagnostics(app: &AppHandle) -> Result<(), String> {
    let directory = log_directory(app)?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("failed to create {}: {error}", directory.display()))?;
    let desktop_path = directory.join("desktop.log");
    rotate_log_if_needed(&desktop_path, MAX_DESKTOP_LOG_BYTES)?;
    let desktop_size = fs::metadata(&desktop_path).map_or(0, |metadata| metadata.len());
    let desktop = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&desktop_path)
        .map_err(|error| format!("failed to open {}: {error}", desktop_path.display()))?;

    let state = app.state::<AppState>();
    let mut diagnostics = state
        .diagnostics
        .lock()
        .expect("diagnostic log lock poisoned");
    *diagnostics = Some(DiagnosticLogState {
        desktop,
        desktop_bytes: desktop_size,
        desktop_truncated: false,
        update: None,
    });
    Ok(())
}

fn begin_update_log(app: &AppHandle) -> Result<(), String> {
    let directory = log_directory(app)?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("failed to create {}: {error}", directory.display()))?;
    cleanup_old_update_logs(&directory)?;

    let path = directory.join(format!(
        "update-{}.log",
        Local::now().format("%Y%m%d-%H%M%S-%3f")
    ));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;

    let state = app.state::<AppState>();
    let mut diagnostics = state
        .diagnostics
        .lock()
        .expect("diagnostic log lock poisoned");
    if let Some(state) = diagnostics.as_mut() {
        if state.update.is_some() {
            return Err("an update diagnostic log is already active".into());
        }
        state.update = Some(UpdateLogState {
            file,
            path: path.clone(),
            started: Instant::now(),
            bytes_written: 0,
            truncated: false,
        });
    } else {
        return Err("diagnostic logging is not initialized".into());
    }
    drop(diagnostics);

    write_update_log(
        app,
        "info",
        format!(
            "DeepSeek Harness update started\n  version: {}\n  os: {}\n  arch: {}\n  pid: {}\n  source: {}\n  runtime: {}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::process::id(),
            writable_source_dir(app)
                .map_or_else(|error| error, |path| path.display().to_string()),
            writable_runtime_dir(app)
                .map_or_else(|error| error, |path| path.display().to_string()),
        ),
    );
    write_environment_snapshot(app);
    Ok(())
}

fn finish_update_log(app: &AppHandle, result: Result<&HarnessStatus, &String>) {
    let (level, message) = match result {
        Ok(status) if status.progress_label == "Update failed" => (
            "error",
            format!(
                "DeepSeek Harness update failed\n  message: {}\n  detail: {}",
                status.message, status.detail
            ),
        ),
        Ok(status) => (
            "info",
            format!(
                "DeepSeek Harness update completed\n  message: {}\n  detail: {}",
                status.message, status.detail
            ),
        ),
        Err(error) => (
            "error",
            format!("DeepSeek Harness update failed\n  error: {error}"),
        ),
    };
    write_update_log(app, level, message);

    let state = app.state::<AppState>();
    let update = {
        let mut diagnostics = state
            .diagnostics
            .lock()
            .expect("diagnostic log lock poisoned");
        diagnostics
        .as_mut()
        .and_then(|state| state.update.take())
    };
    if let Some(update) = update {
        emit_log(
            app,
            "info",
            format!(
                "Update diagnostic log: {} ({} ms, {} bytes{})",
                update.path.display(),
                update.started.elapsed().as_millis(),
                update.bytes_written,
                if update.truncated {
                    ", truncated at size limit"
                } else {
                    ""
                }
            ),
        );
    }
}

fn write_environment_snapshot(app: &AppHandle) {
    let mut lines = Vec::new();
    for key in [
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
    ] {
        let value = std::env::var(key).unwrap_or_else(|_| "(not set)".into());
        lines.push(format!(
            "  {key}={}",
            if key.ends_with("PROXY") {
                redact_url_password(&value)
            } else {
                value
            }
        ));
    }
    write_update_log(app, "info", format!("Environment:\n{}", lines.join("\n")));
}

fn write_update_log(app: &AppHandle, level: &'static str, line: impl AsRef<str>) {
    let path = update_log_path(app);
    write_log_path(app.clone(), path.as_deref(), level, line.as_ref().to_string());
}

fn update_log_path(app: &AppHandle) -> Option<PathBuf> {
    let state = app.state::<AppState>();
    let diagnostics = state
        .diagnostics
        .lock()
        .expect("diagnostic log lock poisoned");
    diagnostics
        .as_ref()
        .and_then(|state| state.update.as_ref())
        .map(|update| update.path.clone())
}

fn write_log_path(
    app: AppHandle,
    update_path: Option<&Path>,
    level: &'static str,
    message: String,
) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f%:z");
    let entry = format!(
        "{timestamp} [{level}] {}\n",
        redact_secrets(&message).trim_end_matches('\n')
    );
    let app_state = app.state::<AppState>();
    let mut diagnostics = app_state
        .diagnostics
        .lock()
        .expect("diagnostic log lock poisoned");
    let Some(state) = diagnostics.as_mut() else {
        return;
    };

    write_bounded_file(
        &mut state.desktop,
        &mut state.desktop_bytes,
        &mut state.desktop_truncated,
        &entry,
        MAX_DESKTOP_LOG_BYTES,
    );
    if let Some(update_path) = update_path {
        if let Some(update) = state.update.as_mut() {
            if update.path == update_path {
                write_bounded_file(
                    &mut update.file,
                    &mut update.bytes_written,
                    &mut update.truncated,
                    &entry,
                    MAX_UPDATE_LOG_BYTES,
                );
            }
        }
    }
}

fn write_bounded_file(
    file: &mut File,
    bytes_written: &mut u64,
    truncated: &mut bool,
    entry: &str,
    limit: u64,
) {
    if *bytes_written >= limit {
        if !*truncated {
            let notice = format!(
                "{} [warn] Log size limit reached; further output is omitted.\n",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f%:z")
            );
            let _ = file.write_all(notice.as_bytes());
            let _ = file.flush();
            *truncated = true;
        }
        return;
    }
    let remaining = limit.saturating_sub(*bytes_written);
    let bytes = entry.as_bytes();
    let accepted = if bytes.len() as u64 > remaining {
        let mut end = remaining as usize;
        while end > 0 && !entry.is_char_boundary(end) {
            end -= 1;
        }
        &bytes[..end]
    } else {
        bytes
    };
    if file.write_all(accepted).is_ok() {
        *bytes_written += accepted.len() as u64;
    }
    let _ = file.flush();
}

fn rotate_log_if_needed(path: &Path, limit: u64) -> Result<(), String> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() <= limit {
        return Ok(());
    }
    let previous = path.with_extension("previous.log");
    if previous.exists() {
        fs::remove_file(&previous).map_err(|error| {
            format!("failed to remove {}: {error}", previous.display())
        })?;
    }
    fs::rename(path, &previous)
        .map_err(|error| format!("failed to rotate {}: {error}", path.display()))
}

fn cleanup_old_update_logs(directory: &Path) -> Result<(), String> {
    let mut logs = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to inspect log entry: {error}"))?;
        let path = entry.path();
        let is_update_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("update-") && name.ends_with(".log"));
        if !is_update_log {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        logs.push((modified, path));
    }
    logs.sort_by_key(|(modified, _)| *modified);
    while logs.len() >= UPDATE_LOG_RETENTION {
        let (_, path) = logs.remove(0);
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn sanitize_log_line(line: &str) -> String {
    let line = strip_ansi_escapes(line);
    let mut output = String::with_capacity(line.len().min(MAX_LOG_LINE_CHARS));
    for character in line.chars().take(MAX_LOG_LINE_CHARS) {
        if character == '\0' {
            continue;
        }
        output.push(character);
    }
    if line.chars().count() > MAX_LOG_LINE_CHARS {
        output.push_str("... [line truncated]");
    }
    output
}

/// Split one captured child-process line into the frames a terminal would have
/// painted in place.
///
/// Tools that render progress (`git`, npm) separate frames with a bare carriage
/// return instead of a newline, so a reader that only splits on `\n` collects a
/// whole progress bar into one entry.
fn progress_frames(line: &str) -> impl Iterator<Item = &str> {
    line.split('\r')
        .map(str::trim_end)
        .filter(|frame| !frame.is_empty())
}

/// Remove ANSI/VT escape sequences from captured child-process output.
///
/// Build tools colorize their output whenever they believe a terminal is
/// attached, and those escape bytes otherwise reach the log files and the status
/// drawer verbatim as `[34m`-style noise. Every consumer renders plain text, so
/// the sequences are dropped here.
fn strip_ansi_escapes(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            // CSI: ESC [ <parameters and intermediates> <final byte @..~>
            '\u{1b}' => match characters.peek() {
                Some('[') => {
                    characters.next();
                    for next in characters.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] <text> terminated by BEL or ESC backslash
                Some(']') => {
                    characters.next();
                    while let Some(next) = characters.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            if characters.peek() == Some(&'\\') {
                                characters.next();
                            }
                            break;
                        }
                    }
                }
                // Two-character escape such as ESC 7 or ESC =.
                Some(_) => {
                    characters.next();
                }
                None => {}
            },
            // Tabs stay meaningful; every other control character is noise.
            '\t' => output.push(character),
            control if control.is_control() => {}
            _ => output.push(character),
        }
    }
    output
}

fn redact_url_password(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_string();
    };
    if url.password().is_some() {
        let _ = url.set_password(Some("REDACTED"));
    }
    url.to_string()
}

fn redact_secrets(value: &str) -> String {
    const KEYS: [&str; 3] = ["token=", "access_token=", "auth="];
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;

    while let Some((index, key)) = KEYS
        .iter()
        .filter_map(|key| remaining.find(key).map(|index| (index, *key)))
        .min_by_key(|(index, _)| *index)
    {
        output.push_str(&remaining[..index + key.len()]);
        output.push_str("[redacted]");
        remaining = &remaining[index + key.len()..];
        let end = remaining
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(character, '&' | '"' | '\'' | ')' | ']' | '}' | ',')
            })
            .unwrap_or(remaining.len());
        remaining = &remaining[end..];
    }

    output.push_str(remaining);
    output
}

fn push_command_output(output: &Arc<Mutex<CommandOutput>>, line: &str) {
    let mut output = output.lock().expect("command output lock poisoned");
    output.lines.push_back(line.to_string());
    while output.lines.len() > COMMAND_OUTPUT_TAIL_LINES {
        output.lines.pop_front();
    }
}

fn command_output_text(output: &Arc<Mutex<CommandOutput>>) -> String {
    let output = output.lock().expect("command output lock poisoned");
    if output.lines.is_empty() {
        "(empty)".into()
    } else {
        output.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

fn indent_log_block(value: &str) -> String {
    value
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn current_status(app: &AppHandle) -> HarnessStatus {
    app.state::<AppState>()
        .status
        .lock()
        .expect("status lock poisoned")
        .clone()
}

fn publish_status(app: &AppHandle, status: HarnessStatus) {
    let state = app.state::<AppState>();
    let mut current = state.status.lock().expect("status lock poisoned");
    let mut status = status;
    status.revision = current.revision.saturating_add(1);
    *current = status.clone();
    drop(current);
    let _ = app.emit("harness-status", status);
}

fn set_phase(
    app: &AppHandle,
    state: &str,
    message: &str,
    detail: &str,
    label: &str,
    progress: u8,
) {
    let update_in_progress = app
        .state::<AppState>()
        .update_in_progress
        .load(Ordering::SeqCst);
    emit_log(
        app,
        "info",
        format!("{label} ({progress}%): {detail}"),
    );
    publish_status(
        app,
        HarnessStatus {
            revision: 0,
            state: state.into(),
            message: message.into(),
            detail: detail.into(),
            port: None,
            url: None,
            progress,
            progress_label: label.into(),
            update_in_progress,
        },
    );
}

/// Picks the status presentation for a build step: the first run is an
/// initialization, an update is an update.
fn report_build_progress(
    app: &AppHandle,
    installing: bool,
    progress: u8,
    label: &str,
    detail: &str,
) {
    if installing {
        set_install_progress(app, progress, label, detail);
    } else {
        set_update_progress(app, progress, label, detail);
    }
}
/// Progress for the first-run download and build. It stays in the
/// `initializing` state so the window keeps showing the setup progress instead
/// of switching to the update presentation, which would also disable the very
/// controls a stuck user needs.
fn set_install_progress(app: &AppHandle, progress: u8, label: &str, detail: &str) {
    emit_log(app, "info", format!("{label} ({progress}%): {detail}"));
    publish_status(
        app,
        HarnessStatus {
            revision: 0,
            state: "initializing".into(),
            message: "Preparing the first-run environment".into(),
            detail: detail.into(),
            port: None,
            url: None,
            progress,
            progress_label: label.into(),
            update_in_progress: false,
        },
    );
}
fn set_update_progress(app: &AppHandle, progress: u8, label: &str, detail: &str) {
    let current = current_status(app);
    emit_log(
        app,
        "info",
        format!("{label} ({progress}%): {detail}"),
    );
    publish_status(
        app,
        HarnessStatus {
            revision: 0,
            state: "updating".into(),
            message: "Updating Harness".into(),
            detail: detail.into(),
            port: current.port,
            url: current.url,
            progress,
            progress_label: label.into(),
            update_in_progress: true,
        },
    );
}

fn set_error(app: &AppHandle, detail: &str, label: &str) {
    emit_log(app, "error", detail.to_string());
    publish_status(
        app,
        HarnessStatus {
            revision: 0,
            state: "error".into(),
            message: label.into(),
            detail: detail.into(),
            port: None,
            url: None,
            progress: 0,
            progress_label: "Error".into(),
            update_in_progress: app
                .state::<AppState>()
                .update_in_progress
                .load(Ordering::SeqCst),
        },
    );
}

fn emit_log(app: &AppHandle, level: &'static str, line: impl Into<String>) {
    let line = line.into();
    let update_path = update_log_path(app);
    write_log_path(app.clone(), update_path.as_deref(), level, line.clone());
    let _ = app.emit(
        "harness-log",
        LogEvent {
            level,
            line,
        },
    );
}

fn quote_argument(argument: &str) -> String {
    if argument.contains(' ') {
        format!("{argument:?}")
    } else {
        argument.to_string()
    }
}

#[cfg(windows)]
fn normalize_windows_path(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    let Some(stripped) = value.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    if stripped.starts_with(r"UNC\") {
        return PathBuf::from(format!(r"\\{}", &stripped[4..]));
    }
    PathBuf::from(stripped)
}

#[cfg(not(windows))]
fn normalize_windows_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn normalize_path_for_command(path: &Path) -> String {
    normalize_windows_path(path).to_string_lossy().to_string()
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        extract_authenticated_url, normalize_path_for_command, normalize_windows_path,
        pnpm_command_args, pnpm_registry_command_args, progress_frames, prune_session_cookies,
        read_package_version, redact_secrets, redact_url_password, runtime_entry_path,
        sanitize_log_line, short_revision, source_is_installed, runtime_is_installed,
        update_needs_rebuild, MAX_LOG_LINE_CHARS,
    };
    use std::path::Path;

    #[test]
    fn skips_the_rebuild_only_for_an_identical_revision() {
        assert!(!update_needs_rebuild("abc123", "abc123", false));
        assert!(update_needs_rebuild("abc123", "def456", false));
        assert!(update_needs_rebuild("abc123", "abc123", true));
        // Unknown revisions must never be treated as "already up to date".
        assert!(update_needs_rebuild("", "abc123", false));
        assert!(update_needs_rebuild("abc123", "", false));
    }

    #[test]
    fn shortens_git_object_names_for_display() {
        assert_eq!(short_revision("0d1f50007f9bca3f52b06e1c3074fa14d5fb0720"), "0d1f500");
        assert_eq!(short_revision("abc"), "abc");
        assert_eq!(short_revision(""), "");
    }
    fn temp_dir(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("dsh-{label}-{}", std::process::id()));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn detects_installed_source_and_runtime_directories() {
        let root = temp_dir("installed-probe");
        let source = root.join("source");
        let runtime = root.join("runtime");

        assert!(!source_is_installed(&source));
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("package.json"), "{}").unwrap();
        assert!(!source_is_installed(&source), "a checkout needs Git metadata");
        std::fs::create_dir_all(source.join(".git")).unwrap();
        assert!(source_is_installed(&source));

        assert!(!runtime_is_installed(&runtime));
        std::fs::create_dir_all(runtime.join("node_modules/@deepseek-ai/dsh/lib")).unwrap();
        std::fs::write(runtime.join("package.json"), "{}").unwrap();
        assert!(!runtime_is_installed(&runtime), "the CLI entry point is required");
        std::fs::write(runtime_entry_path(&runtime), "// entry").unwrap();
        assert!(runtime_is_installed(&runtime));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolves_the_runtime_entry_point_inside_a_deployment() {
        let runtime = Path::new("C:/dsh/runtime");
        assert_eq!(
            runtime_entry_path(runtime),
            runtime
                .join("node_modules")
                .join("@deepseek-ai")
                .join("dsh")
                .join("lib")
                .join("bin.js")
        );
    }
    #[test]
    fn reads_harness_version_from_package_manifest() {
        let directory = std::env::temp_dir().join(format!("dsh-version-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let manifest = directory.join("package.json");
        std::fs::write(
            &manifest,
            r#"{ "name": "@deepseek-ai/dsh", "version": "0.1.6-alpha.1" }"#,
        )
        .unwrap();
        assert_eq!(
            read_package_version(&manifest).as_deref(),
            Some("0.1.6-alpha.1")
        );

        std::fs::write(&manifest, r#"{ "version": "" }"#).unwrap();
        assert_eq!(read_package_version(&manifest), None);
        assert_eq!(read_package_version(&directory.join("missing.json")), None);

        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn extracts_harness_launch_url() {
        let line = "dsh web: http://127.0.0.1:39082/?token=abc_DEF-123";
        assert_eq!(
            extract_authenticated_url(line, 39082).as_deref(),
            Some("http://127.0.0.1:39082/?token=abc_DEF-123")
        );
    }

    #[test]
    fn ignores_unauthenticated_or_unrelated_urls() {
        assert_eq!(
            extract_authenticated_url("http://127.0.0.1:39082/", 39082),
            None
        );
        assert_eq!(
            extract_authenticated_url("http://127.0.0.1:39082/?token=abc", 39081),
            None
        );
    }

    #[test]
    fn redacts_tokens_from_log_output() {
        assert_eq!(
            redact_secrets("http://127.0.0.1:39082/?token=abc_DEF-123&view=main"),
            "http://127.0.0.1:39082/?token=[redacted]&view=main"
        );
        assert_eq!(
            redact_secrets("access_token=secret-value Authorization: Bearer hidden"),
            "access_token=[redacted] Authorization: Bearer hidden"
        );
    }

    #[test]
    fn redacts_proxy_passwords() {
        assert_eq!(
            redact_url_password("http://user:secret@127.0.0.1:8080"),
            "http://user:REDACTED@127.0.0.1:8080/"
        );
    }

    #[test]
    fn limits_oversized_log_lines() {
        let line = "x".repeat(MAX_LOG_LINE_CHARS + 10);
        let sanitized = sanitize_log_line(&line);
        assert!(sanitized.ends_with("... [line truncated]"));
        assert!(sanitized.len() > MAX_LOG_LINE_CHARS);
    }

    #[test]
    fn strips_ansi_escapes_from_log_lines() {
        assert_eq!(
            sanitize_log_line("\u{1b}[34mℹ\u{1b}[39m tsdown v0.22.2 powered by \u{1b}[91mrolldown\u{1b}[39m"),
            "ℹ tsdown v0.22.2 powered by rolldown"
        );
        assert_eq!(
            sanitize_log_line("\u{1b}[32m> Moving conpty.dll...\u{1b}[0m"),
            "> Moving conpty.dll..."
        );
        // OSC hyperlinks and two-character escapes are dropped as well.
        assert_eq!(
            sanitize_log_line("see \u{1b}]8;;https://example.com\u{7}example\u{1b}]8;;\u{7} now"),
            "see example now"
        );
        assert_eq!(sanitize_log_line("plain\u{1b}7text"), "plaintext");
    }

    #[test]
    fn splits_progress_frames_on_carriage_returns() {
        let line = "Updating files:   9% (962/10319)\rUpdating files:  10% (1032/10319)\r";
        assert_eq!(
            progress_frames(line).collect::<Vec<_>>(),
            vec![
                "Updating files:   9% (962/10319)",
                "Updating files:  10% (1032/10319)",
            ]
        );
        assert_eq!(progress_frames("").count(), 0);
        assert_eq!(
            progress_frames("single frame").collect::<Vec<_>>(),
            vec!["single frame"]
        );
    }

    #[test]
    fn removes_persisted_session_cookie_files() {
        let root = std::env::temp_dir().join(format!(
            "dsh-cookie-prune-{}",
            std::process::id()
        ));
        let network = root.join("Default").join("Network");
        std::fs::create_dir_all(&network).expect("create fixture");
        let cookies = network.join("Cookies");
        let journal = network.join("Cookies-journal");
        std::fs::write(&cookies, b"stale").expect("write fixture");
        std::fs::write(&journal, b"stale").expect("write fixture");

        let removed = prune_session_cookies(&root);

        assert_eq!(removed.len(), 2);
        assert!(!cookies.exists());
        assert!(!journal.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn places_pnpm_config_before_the_subcommand() {
        assert_eq!(
            pnpm_command_args(
                Path::new("pnpm.cjs"),
                vec!["run".into(), "build".into()],
            ),
            vec![
                "pnpm.cjs",
                "--config.confirmModulesPurge=false",
                "--config.verify-deps-before-run=false",
                "run",
                "build",
            ]
        );
    }

    #[test]
    fn only_adds_registry_to_dependency_commands() {
        assert_eq!(
            pnpm_registry_command_args(
                Path::new("pnpm.cjs"),
                "https://registry.npmmirror.com/",
                vec!["install".into(), "--frozen-lockfile".into()],
            ),
            vec![
                "pnpm.cjs",
                "--config.confirmModulesPurge=false",
                "--config.verify-deps-before-run=false",
                "--registry=https://registry.npmmirror.com/",
                "install",
                "--frozen-lockfile",
            ]
        );
    }

    #[test]
    fn removes_windows_verbatim_prefixes() {
        assert_eq!(
            normalize_windows_path(Path::new(r"\\?\D:\code\harness\node.exe")),
            Path::new(r"D:\code\harness\node.exe")
        );
        assert_eq!(
            normalize_windows_path(Path::new(r"\\?\UNC\server\share\node.exe")),
            Path::new(r"\\server\share\node.exe")
        );
    }

    #[test]
    fn normalizes_explicit_path_arguments_only() {
        assert_eq!(
            normalize_path_for_command(Path::new(r"\\?\D:\portable\node.exe")),
            r"D:\portable\node.exe"
        );
    }
}
