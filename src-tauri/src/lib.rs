use serde::Serialize;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, State, WebviewBuilder,
    WebviewUrl, WindowEvent,
};
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader as TokioBufReader},
    process::Command as AsyncCommand,
};

const TOP_BAR_HEIGHT: f64 = 64.0;
const DRAWER_HEIGHT: f64 = 300.0;

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
            message: "Preparing the local runtime".into(),
            detail: "The first launch can take a moment while files are prepared.".into(),
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
    child: Mutex<Option<CommandChild>>,
    authenticated_url: Mutex<Option<String>>,
    generation: AtomicU64,
    update_in_progress: AtomicBool,
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
async fn update_harness(app: AppHandle, state: State<'_, AppState>) -> Result<HarnessStatus, String> {
    if state.update_in_progress.swap(true, Ordering::SeqCst) {
        return Err("An update is already in progress.".into());
    }

    let result = perform_update(app.clone()).await;
    state.update_in_progress.store(false, Ordering::SeqCst);

    match result {
        Ok(status) => {
            publish_status(&app, status.clone());
            Ok(status)
        }
        Err(error) => {
            set_error(&app, &error, "Update failed");
            Err(error)
        }
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            open_harness,
            restart_harness,
            update_harness,
            set_drawer_open
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let init_handle = handle.clone();
                let init_result =
                    tauri::async_runtime::spawn_blocking(move || init_harness_dirs(&init_handle))
                        .await;

                match init_result {
                    Ok(Ok(())) => {
                        if let Err(error) = start_service(handle.clone()).await {
                            set_error(&handle, &error, "Startup failed");
                        }
                    }
                    Ok(Err(error)) => set_error(&handle, &error, "Initialization failed"),
                    Err(error) => set_error(
                        &handle,
                        &format!("Initialization task failed: {error}"),
                        "Initialization failed",
                    ),
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
        .build(tauri::generate_context!())
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

    let source_dir = writable_source_dir(&app)?;
    if !source_dir.join(".git").exists() {
        repair_source_checkout(&app)?;
    }

    let port = find_free_port()?;
    let url = format!("http://127.0.0.1:{port}");
    let runtime_entry = writable_runtime_dir(&app)?
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    if !runtime_entry.exists() {
        return Err(format!(
            "Harness runtime entry is missing at {}",
            runtime_entry.display()
        ));
    }

    let pnpm_home = resource_path(&app, "runtime/pnpm")?;
    let sidecar = app
        .shell()
        .sidecar("node")
        .map_err(|error| format!("failed to resolve bundled Node.js: {error}"))?
        .args([
            runtime_entry.to_string_lossy().to_string(),
            "web".into(),
            "--port".into(),
            port.to_string(),
            "--no-open".into(),
        ])
        .current_dir(&source_dir)
        .env("PNPM_HOME", pnpm_home.to_string_lossy().to_string());

    let (mut receiver, child) = sidecar
        .spawn()
        .map_err(|error| format!("failed to start Harness: {error}"))?;
    let pid = child.pid();
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
    let log_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = receiver.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    let line = String::from_utf8_lossy(&line).trim_end().to_string();
                    if let Some(authenticated_url) = extract_authenticated_url(&line, port) {
                        *log_handle
                            .state::<AppState>()
                            .authenticated_url
                            .lock()
                            .expect("authenticated URL lock poisoned") =
                            Some(authenticated_url);
                    }
                    emit_log(
                        &log_handle,
                        "info",
                        line,
                    );
                }
                CommandEvent::Stderr(line) => {
                    emit_log(
                        &log_handle,
                        "warn",
                        String::from_utf8_lossy(&line).trim_end().to_string(),
                    );
                }
                CommandEvent::Error(error) => {
                    emit_log(&log_handle, "error", error);
                }
                CommandEvent::Terminated(payload) => {
                    let state = log_handle.state::<AppState>();
                    if state.generation.load(Ordering::SeqCst) == generation {
                        state.child.lock().expect("child lock poisoned").take();
                        let message = match payload.code {
                            Some(code) => format!("Harness stopped unexpectedly (exit code {code})."),
                            None => "Harness stopped unexpectedly.".to_string(),
                        };
                        set_error(&log_handle, &message, "Harness stopped");
                    }
                    break;
                }
                _ => {}
            }
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

async fn perform_update(app: AppHandle) -> Result<HarnessStatus, String> {
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

    let build_result = async {
        ensure_update_checkout(&app, &source_dir)?;
        let git = resource_path(&app, "runtime/git/cmd/git.exe")?;
        let node = resource_path(&app, "runtime/node/node.exe")?;
        let pnpm = resource_path(&app, "runtime/pnpm/pnpm.cjs")?;
        let pnpm_home = resource_path(&app, "runtime/pnpm")?;

        run_command(
            &app,
            &git,
            strings(["fetch", "--prune", "--depth", "1", "origin", "master"]),
            &source_dir,
            Vec::new(),
            "git fetch",
        )
        .await?;
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

        set_update_progress(
            &app,
            24,
            "Installing dependencies",
            "Resolving the locked Harness workspace.",
        );
        run_pnpm(
            &app,
            &node,
            &pnpm,
            &source_dir,
            &pnpm_home,
            strings(["install", "--frozen-lockfile"]),
            "pnpm install",
        )
        .await?;

        set_update_progress(
            &app,
            52,
            "Cleaning build state",
            "Removing stale incremental build artifacts.",
        );
        run_pnpm(
            &app,
            &node,
            &pnpm,
            &source_dir,
            &pnpm_home,
            strings(["run", "clean"]),
            "pnpm run clean",
        )
        .await?;

        set_update_progress(
            &app,
            61,
            "Building Harness",
            "Compiling the host, client, and Web application.",
        );
        run_pnpm(
            &app,
            &node,
            &pnpm,
            &source_dir,
            &pnpm_home,
            strings(["run", "build"]),
            "pnpm run build",
        )
        .await?;

        set_update_progress(
            &app,
            88,
            "Packaging runtime",
            "Creating a production-only dependency tree.",
        );
        remove_directory_if_exists(&next_runtime)?;
        run_pnpm(
            &app,
            &node,
            &pnpm,
            &source_dir,
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
                next_runtime.to_string_lossy().to_string(),
            ],
            "pnpm deploy",
        )
        .await?;
        let repair_script = resource_path(&app, "tools/repair-runtime.mjs")?;
        run_command(
            &app,
            &node,
            vec![
                repair_script.to_string_lossy().to_string(),
                source_dir.to_string_lossy().to_string(),
                next_runtime.to_string_lossy().to_string(),
            ],
            &source_dir,
            Vec::new(),
            "runtime dependency repair",
        )
        .await?;
        prepare_deployed_runtime(&app, &node, &next_runtime).await?;
        remove_source_node_modules(&source_dir)?;

        let entry = next_runtime
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        if !entry.exists() {
            return Err(format!(
                "New runtime entry is missing at {}",
                entry.display()
            ));
        }
        Ok::<(), String>(())
    }
    .await;

    if let Err(error) = build_result {
        let _ = remove_directory_if_exists(&next_runtime);
        return recover_after_update_failure(app, error).await;
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
            if let Err(rollback_error) =
                restore_previous_runtime(&active_runtime, &backup_runtime)
            {
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
                koffi.to_string_lossy().to_string(),
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

fn init_harness_dirs(app: &AppHandle) -> Result<(), String> {
    let source = resource_path(app, "runtime/harness-source.zip")?;
    let runtime = resource_path(app, "runtime/harness-runtime.zip")?;
    let writable_root = writable_harness_root(app)?;
    fs::create_dir_all(&writable_root)
        .map_err(|error| format!("failed to create {}: {error}", writable_root.display()))?;

    install_resource_archive(&source, &writable_root.join("source"), "source")?;
    install_resource_archive(&runtime, &writable_root.join("runtime"), "runtime")?;
    Ok(())
}

fn install_resource_archive(
    archive_path: &Path,
    destination: &Path,
    name: &str,
) -> Result<(), String> {
    let marker = destination.join(".deepseek-harness-ready");
    if marker.exists() {
        return Ok(());
    }

    let parent = destination
        .parent()
        .ok_or_else(|| format!("invalid destination {}", destination.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let staging = parent.join(format!(".{name}-installing"));
    remove_directory_if_exists(&staging)?;
    extract_zip(archive_path, &staging)
        .map_err(|error| format!("failed to extract bundled {name}: {error}"))?;
    fs::write(&staging.join(".deepseek-harness-ready"), b"ready\n")
        .map_err(|error| format!("failed to write {name} marker: {error}"))?;
    remove_directory_if_exists(destination)?;
    fs::rename(&staging, destination).map_err(|error| {
        format!(
            "failed to activate bundled {name} at {}: {error}",
            destination.display()
        )
    })
}

fn extract_zip(archive_path: &Path, destination: &Path) -> Result<(), String> {
    let file = fs::File::open(archive_path)
        .map_err(|error| format!("failed to open {}: {error}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("failed to read {}: {error}", archive_path.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("failed to read ZIP entry {index}: {error}"))?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)
                .map_err(|error| format!("failed to create {}: {error}", output.display()))?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        let mut output_file = fs::File::create(&output)
            .map_err(|error| format!("failed to create {}: {error}", output.display()))?;
        std::io::copy(&mut entry, &mut output_file)
            .map_err(|error| format!("failed to extract {}: {error}", output.display()))?;
    }
    Ok(())
}

fn repair_source_checkout(app: &AppHandle) -> Result<(), String> {
    let source = resource_path(app, "runtime/harness-source.zip")?;
    let destination = writable_source_dir(app)?;
    remove_directory_if_exists(&destination)?;
    extract_zip(&source, &destination)?;
    Ok(())
}

fn ensure_update_checkout(app: &AppHandle, source_dir: &Path) -> Result<(), String> {
    if !source_dir.exists() || !source_dir.join("package.json").exists() {
        repair_source_checkout(app)?;
    }
    if !source_dir.join(".git").exists() {
        repair_source_checkout(app)?;
    }
    Ok(())
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
    let mut command_args = Vec::with_capacity(args.len() + 1);
    command_args.push(pnpm.to_string_lossy().to_string());
    command_args.extend(args);
    run_command(
        app,
        node,
        command_args,
        cwd,
        vec![(
            "PNPM_HOME".into(),
            pnpm_home.to_string_lossy().to_string(),
        )],
        label,
    )
    .await
}

async fn run_command(
    app: &AppHandle,
    program: &Path,
    args: Vec<String>,
    cwd: &Path,
    environment: Vec<(OsString, String)>,
    label: &str,
) -> Result<(), String> {
    emit_log(
        app,
        "info",
        format!(
            "{}: {} {}",
            label,
            program.display(),
            args.iter()
                .map(|argument| quote_argument(argument))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    );

    let mut command = AsyncCommand::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CI", "1")
        .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0");
    for (key, value) in environment {
        command.env(key, value);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start {label}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("failed to capture stdout for {label}"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("failed to capture stderr for {label}"))?;

    let stdout_app = app.clone();
    let stdout_task = tauri::async_runtime::spawn(async move {
        let mut lines = TokioBufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            emit_log(&stdout_app, "info", line);
        }
    });
    let stderr_app = app.clone();
    let stderr_task = tauri::async_runtime::spawn(async move {
        let mut lines = TokioBufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            emit_log(&stderr_app, "warn", line);
        }
    });

    let status = child
        .wait()
        .await
        .map_err(|error| format!("failed while waiting for {label}: {error}"))?;
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label} failed with {}",
            status
                .code()
                .map_or_else(|| "an unknown status".to_string(), |code| format!("exit code {code}"))
        ))
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
        let pid = child.pid();
        emit_log(app, "info", format!("Stopping Harness process {pid}"));
        kill_process_tree(pid);
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

fn set_update_progress(app: &AppHandle, progress: u8, label: &str, detail: &str) {
    let current = current_status(app);
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
    let _ = app.emit(
        "harness-log",
        LogEvent {
            level,
            line: line.into(),
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

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::extract_authenticated_url;

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
}
