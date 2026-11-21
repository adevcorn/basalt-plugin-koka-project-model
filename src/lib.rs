//! Project-model, build-system, and test-runner plugin for Koka.
//!
//! Emits the shared generic project-model JSON schema under
//! `project-model:koka`, and exposes `koka -c` (check/build) and
//! `koka -e` (test/run) as review actions.

#![allow(static_mut_refs)]
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use basalt_plugin_sdk::prelude::*;

basalt_plugin_meta! {
    name:              "koka-project-model",
    version:           "0.1.0",
    hook_flags:        CAP_PROJECT_MODEL | CAP_REVIEW_ACTIONS | CAP_BUILD_SYSTEM | CAP_TEST_RUNNER | CAP_SHADOW_EXCLUDES,
    provides:          "project-model:koka",
    requires:          "",
    file_globs:        "",
    activates_on:      "koka.toml\nkoka.yaml\n**/koka.toml\n**/koka.yaml\n**/*.kk",
    activation_events: "",
}

/// Koka compiles into `.koka/`; it is reproducible output, not source.
#[basalt_plugin]
fn shadow_excludes(_root: &str) -> Vec<u8> {
    br#"["**/.koka/**"]"#.to_vec()
}

#[cfg(target_arch = "wasm32")]
extern "C" {
    fn basalt_read_file(path_ptr: i32, path_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    fn basalt_list_files(root_ptr: i32, root_len: i32, out_ptr: i32, out_cap: i32) -> i32;
}

#[cfg(not(target_arch = "wasm32"))]
#[no_mangle]
unsafe extern "C" fn basalt_read_file(
    _path_ptr: i32,
    _path_len: i32,
    _out_ptr: i32,
    _out_cap: i32,
) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
#[no_mangle]
unsafe extern "C" fn basalt_list_files(
    _root_ptr: i32,
    _root_len: i32,
    _out_ptr: i32,
    _out_cap: i32,
) -> i32 {
    -1
}

const FILE_BUF_SIZE: usize = 4 * 1024 * 1024;
const LIST_BUF_SIZE: usize = 8 * 1024 * 1024;

static mut FILE_BUF: [u8; FILE_BUF_SIZE] = [0u8; FILE_BUF_SIZE];
static mut LIST_BUF: [u8; LIST_BUF_SIZE] = [0u8; LIST_BUF_SIZE];

#[derive(Debug, Default, Clone)]
struct KokaProject {
    name: String,
    version: Option<String>,
    main_file: Option<String>,
    test_file: Option<String>,
    include_dirs: Vec<String>,
}

#[basalt_plugin]
fn build_project_model(root: &str) -> Vec<u8> {
    let Some((manifest, project, prefix)) = discover_project(root) else {
        return Vec::new();
    };
    emit_model(root, &manifest, &project, &prefix).into_bytes()
}

fn discover_project(root: &str) -> Option<(String, KokaProject, String)> {
    let files = list_host_files(root);
    let mut manifest_path = None;
    let mut best_depth = usize::MAX;
    let mut kk_files = Vec::new();

    for rel in files {
        let rel = normalize_rel_path(&rel);
        let fname = file_name(&rel);
        if fname == "koka.toml" || fname == "koka.yaml" {
            let depth = rel.matches('/').count();
            if depth < best_depth {
                best_depth = depth;
                manifest_path = Some(join_path(root, &rel));
            }
        } else if rel.ends_with(".kk") && !rel.contains(".koka/") {
            kk_files.push(rel);
        }
    }

    if let Some(manifest) = manifest_path {
        let contents = read_host_file(&manifest).unwrap_or_default();
        let mut proj = parse_manifest(&contents);
        if proj.name.is_empty() {
            proj.name = dir_basename(&parent_dir(&manifest)).to_string();
        }
        let pkg_root = parent_dir(&manifest);
        let prefix = relative_to(root, &pkg_root);
        fill_entry_files(&mut proj, &kk_files);
        return Some((manifest, proj, prefix));
    }

    if kk_files.is_empty() {
        return None;
    }

    let mut proj = KokaProject {
        name: dir_basename(root).to_string(),
        ..Default::default()
    };
    fill_entry_files(&mut proj, &kk_files);
    let primary = proj
        .main_file
        .clone()
        .or_else(|| kk_files.first().cloned())
        .unwrap_or_default();
    Some((join_path(root, &primary), proj, String::new()))
}

fn fill_entry_files(proj: &mut KokaProject, kk_files: &[String]) {
    if proj.main_file.is_none() {
        proj.main_file = kk_files
            .iter()
            .find(|f| {
                let n = file_name(f);
                n == "main.kk" || n == format!("{}.kk", proj.name)
            })
            .or_else(|| kk_files.first())
            .cloned();
    }
    if proj.test_file.is_none() {
        proj.test_file = kk_files
            .iter()
            .find(|f| {
                f.starts_with("test/")
                    || f.contains("/test/")
                    || file_name(f).ends_with("_test.kk")
                    || file_name(f).starts_with("test_")
            })
            .cloned();
    }
}

fn parse_string_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(|item| item.trim().trim_matches('"').trim_matches('\'').trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn parse_manifest(contents: &str) -> KokaProject {
    let mut proj = KokaProject::default();
    let mut in_include_block = false;
    for raw in contents.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if in_include_block {
            if let Some(item) = line.strip_prefix('-') {
                let val = item.trim().trim_matches('"').trim_matches('\'').trim();
                if !val.is_empty() {
                    proj.include_dirs.push(val.to_string());
                }
                continue;
            }
            if line.ends_with(']') {
                let before = line.trim_end_matches(']');
                proj.include_dirs.extend(parse_string_list(before));
                in_include_block = false;
                continue;
            }
            if !line.contains('=') && !line.contains(':') {
                proj.include_dirs.extend(parse_string_list(line));
                continue;
            }
            in_include_block = false;
        }
        let sep = if line.contains('=') { '=' } else { ':' };
        let Some((key, val)) = line.split_once(sep) else {
            continue;
        };
        let key = key.trim();
        let raw_val = val.trim();
        let val = raw_val.trim_matches('"').trim_matches('\'').to_string();
        match key {
            "name" => proj.name = val,
            "version" => proj.version = Some(val),
            "main" | "entry" => proj.main_file = Some(val),
            "test" => proj.test_file = Some(val),
            "include" | "includes" | "include_dirs" | "include-dirs" => {
                if raw_val.is_empty() || (raw_val.starts_with('[') && !raw_val.ends_with(']')) {
                    in_include_block = true;
                    let partial = raw_val.trim_start_matches('[');
                    proj.include_dirs.extend(parse_string_list(partial));
                } else {
                    proj.include_dirs.extend(parse_string_list(raw_val));
                }
            }
            _ => {}
        }
    }
    proj
}

fn include_args(proj: &KokaProject) -> Vec<String> {
    proj.include_dirs
        .iter()
        .map(|d| format!("-i{d}"))
        .collect()
}

fn emit_model(root: &str, manifest: &str, project: &KokaProject, prefix: &str) -> String {
    let project_id = sanitize_id(&project.name);
    let is_app = project.main_file.is_some();
    let kind = if is_app { "app" } else { "library" };
    let subtype = if is_app {
        "koka-application"
    } else {
        "koka-package"
    };
    let capabilities = if is_app {
        "\"is-executable\""
    } else {
        "\"is-library\""
    };

    let source_roots = format!("\"{}\"", escape_json(&with_prefix(prefix, "src/")));
    let test_roots = format!("\"{}\"", escape_json(&with_prefix(prefix, "test/")));
    let build_roots = format!("\"{}\"", escape_json(&with_prefix(prefix, ".koka/")));

    let target = format!(
        "{{\"id\":\"{id}\",\"name\":\"{name}\",\"kind\":\"{kind}\",\"subtype\":\"{subtype}\",\"capabilities\":[{caps}],\"language\":\"koka\",\"source_roots\":[{src}],\"test_roots\":[{test}],\"build_roots\":[{bld}]}}",
        id = escape_json(&project_id),
        name = escape_json(&project.name),
        kind = kind,
        subtype = subtype,
        caps = capabilities,
        src = source_roots,
        test = test_roots,
        bld = build_roots,
    );

    let proj_json = format!(
        "{{\"id\":\"{id}\",\"name\":\"{name}\",\"kind\":\"{kind}\",\"subtype\":\"{subtype}\",\"capabilities\":[{caps}],\"manifest_path\":\"{manifest}\",\"source_roots\":[{src}],\"test_roots\":[{test}],\"build_roots\":[{bld}],\"targets\":[{target}]}}",
        id = escape_json(&project_id),
        name = escape_json(&project.name),
        kind = kind,
        subtype = subtype,
        caps = capabilities,
        manifest = escape_json(manifest),
        src = source_roots,
        test = test_roots,
        bld = build_roots,
        target = target,
    );

    let owners = [
        owner_json(&with_prefix(prefix, "src/"), "prefix", &project.name, &project_id, "source"),
        owner_json(&with_prefix(prefix, "test/"), "prefix", &project.name, &project_id, "test"),
        owner_json(&with_prefix(prefix, ".koka/"), "prefix", &project.name, &project_id, "build"),
    ];

    format!(
        "{{\"schema_version\":1,\"ecosystem\":\"koka\",\"root\":\"{root}\",\"display_name\":\"{name}\",\"primary_manifest_path\":\"{manifest}\",\"projects\":[{proj}],\"owners\":[{owners}]}}",
        root = escape_json(root),
        name = escape_json(&project.name),
        manifest = escape_json(manifest),
        proj = proj_json,
        owners = owners.join(","),
    )
}

fn owner_json(path: &str, match_kind: &str, label: &str, project_id: &str, kind: &str) -> String {
    format!(
        "{{\"path\":\"{}\",\"match_kind\":\"{}\",\"label\":\"{}\",\"project_id\":\"{}\",\"target_id\":\"{}\",\"kind\":\"{}\"}}",
        escape_json(path),
        escape_json(match_kind),
        escape_json(label),
        escape_json(project_id),
        escape_json(project_id),
        escape_json(kind),
    )
}

// ── Review actions ──────────────────────────────────────────────────────────

#[basalt_plugin]
fn review_actions(workspace_root: &str, _session_workspace: &str) -> Vec<ReviewActionDescriptor> {
    let Some((_, proj, _)) = discover_project(workspace_root) else {
        return Vec::new();
    };
    let inc = include_args(&proj);
    let inc_prefix = if inc.is_empty() {
        String::new()
    } else {
        format!("{} ", inc.join(" "))
    };
    let mut actions = Vec::new();
    if let Some(entry) = proj.main_file {
        actions.push(ReviewActionDescriptor {
            id: "koka-check".into(),
            title: "Type-check Koka Project".into(),
            kind: ReviewActionKind::Build,
            ecosystem: "koka".into(),
            command_preview: format!("koka {inc_prefix}-l {entry}"),
            mutates_workspace: false,
            priority: 100,
        });
    }
    if let Some(test_entry) = proj.test_file {
        actions.push(ReviewActionDescriptor {
            id: "koka-test".into(),
            title: "Run Koka Tests".into(),
            kind: ReviewActionKind::Test,
            ecosystem: "koka".into(),
            command_preview: format!("koka {inc_prefix}-e {test_entry}"),
            mutates_workspace: false,
            priority: 100,
        });
    }
    actions
}

#[basalt_plugin]
fn review_action_plan(
    action_id: &str,
    workspace_root: &str,
    _session_workspace: &str,
) -> Option<ReviewActionExecutionPlan> {
    let (_, proj, _) = discover_project(workspace_root)?;
    let mut args = include_args(&proj);
    match action_id {
        "koka-check" => {
            let entry = proj.main_file?;
            args.push("-l".into());
            args.push(entry);
            Some(ReviewActionExecutionPlan {
                executable: "koka".into(),
                args,
                env: Vec::new(),
                cwd_mode: ReviewActionCwdMode::SessionWorkspace,
                output_category: "build".into(),
            })
        }
        "koka-test" => {
            let entry = proj.test_file?;
            args.push("-e".into());
            args.push(entry);
            Some(ReviewActionExecutionPlan {
                executable: "koka".into(),
                args,
                env: Vec::new(),
                cwd_mode: ReviewActionCwdMode::SessionWorkspace,
                output_category: "test".into(),
            })
        }
        _ => None,
    }
}

/// Parse Koka compiler diagnostic lines such as:
/// `src/main.kk(12,5): error: type mismatch`
pub fn parse_koka_diagnostic(line: &str) -> Option<(String, u32, u32, bool, String)> {
    let trimmed = line.trim();
    let (loc_part, msg_part) = trimmed.split_once("): ")?;
    let (path, coords) = loc_part.rsplit_once('(')?;
    let (line_s, col_s) = coords.split_once(',')?;
    let line_no = line_s.trim().parse::<u32>().ok()?;
    let col_no = col_s.trim().parse::<u32>().ok()?;
    let is_warning = msg_part.starts_with("warning:");
    Some((
        path.trim().to_string(),
        line_no,
        col_no,
        is_warning,
        msg_part.trim().to_string(),
    ))
}

/// Detect Koka runtime test failure lines that exit with status 0
/// (e.g. `uncaught exception: FAIL: ...` or `FAIL: ...`).
pub fn parse_koka_test_failure(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("uncaught exception:")
        || trimmed.starts_with("FAIL:")
        || trimmed.starts_with("FAILED:")
    {
        return Some(trimmed.to_string());
    }
    None
}

#[basalt_plugin]
fn review_action_parse_line(
    action_id: &str,
    line: &[u8],
    state: &[u8],
) -> (Vec<u8>, Vec<AgentEvent>) {
    let Ok(line_str) = core::str::from_utf8(line) else {
        return (state.to_vec(), Vec::new());
    };
    let mut events = Vec::new();
    if let Some((path, line_no, col_no, is_warning, msg)) = parse_koka_diagnostic(line_str) {
        let vendor_id = format!("koka-diag-{path}-{line_no}-{col_no}");
        events.push(AgentEvent::NewEntry {
            vendor_id: vendor_id.clone(),
            tool: format!("{path}:{line_no}:{col_no}"),
            category: "diagnostic".into(),
            raw_cmd: line_str.trim().to_string(),
            file_paths: vec![path],
        });
        events.push(AgentEvent::CloseEntry {
            vendor_id,
            exit_code: if is_warning { 0 } else { 1 },
            output_lines: vec![msg],
        });
    } else if action_id == "koka-test" {
        if let Some(fail_msg) = parse_koka_test_failure(line_str) {
            let vendor_id = format!("koka-test-fail-{}", fail_msg.len());
            events.push(AgentEvent::NewEntry {
                vendor_id: vendor_id.clone(),
                tool: "koka-test".into(),
                category: "test".into(),
                raw_cmd: fail_msg.clone(),
                file_paths: Vec::new(),
            });
            events.push(AgentEvent::CloseEntry {
                vendor_id,
                exit_code: 1,
                output_lines: vec![fail_msg],
            });
        }
    }
    (state.to_vec(), events)
}

// ── Host I/O & path helpers ─────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn read_host_file(path: &str) -> Option<String> {
    let buf: &mut [u8; FILE_BUF_SIZE] = unsafe { &mut *core::ptr::addr_of_mut!(FILE_BUF) };
    let n = unsafe {
        basalt_read_file(
            path.as_ptr() as i32,
            path.len() as i32,
            buf.as_mut_ptr() as i32,
            FILE_BUF_SIZE as i32,
        )
    };
    if n <= 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(not(target_arch = "wasm32"))]
fn read_host_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(target_arch = "wasm32")]
fn list_host_files(root: &str) -> Vec<String> {
    let buf: &mut [u8; LIST_BUF_SIZE] = unsafe { &mut *core::ptr::addr_of_mut!(LIST_BUF) };
    let n = unsafe {
        basalt_list_files(
            root.as_ptr() as i32,
            root.len() as i32,
            buf.as_mut_ptr() as i32,
            LIST_BUF_SIZE as i32,
        )
    };
    if n <= 0 {
        return Vec::new();
    }
    String::from_utf8_lossy(&buf[..n as usize])
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn list_host_files(_root: &str) -> Vec<String> {
    Vec::new()
}

fn normalize_rel_path(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_string()
}

fn file_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn parent_dir(p: &str) -> String {
    let norm = p.replace('\\', "/");
    match norm.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_string(),
        _ => ".".to_string(),
    }
}

fn dir_basename(p: &str) -> &str {
    let norm = p.trim_end_matches(&['/', '\\'][..]);
    norm.rsplit(&['/', '\\'][..]).next().unwrap_or("koka-project")
}

fn join_path(a: &str, b: &str) -> String {
    if a.is_empty() || a == "." {
        b.to_string()
    } else {
        format!("{}/{}", a.trim_end_matches(&['/', '\\'][..]), b.trim_start_matches('/'))
    }
}

fn relative_to(root: &str, dir: &str) -> String {
    let r = root.replace('\\', "/");
    let d = dir.replace('\\', "/");
    d.strip_prefix(&r)
        .unwrap_or(&d)
        .trim_start_matches('/')
        .to_string()
}

fn with_prefix(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{}/{}", prefix.trim_end_matches('/'), suffix)
    }
}

fn sanitize_id(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    if out.is_empty() { "koka".to_string() } else { out }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_koka_manifest() {
        let proj = parse_manifest(
            "name = \"hello-koka\"\nversion = \"0.1.0\"\nmain = \"src/main.kk\"\ninclude = [\"src\", \"test\"]\n",
        );
        assert_eq!(proj.name, "hello-koka");
        assert_eq!(proj.version.as_deref(), Some("0.1.0"));
        assert_eq!(proj.main_file.as_deref(), Some("src/main.kk"));
        assert_eq!(proj.include_dirs, vec!["src", "test"]);
        assert_eq!(include_args(&proj), vec!["-isrc", "-itest"]);
    }

    #[test]
    fn parses_koka_compiler_diagnostic() {
        let line = "src/main.kk(12,5): error: type mismatch";
        let (path, l, c, warn, msg) = parse_koka_diagnostic(line).unwrap();
        assert_eq!(path, "src/main.kk");
        assert_eq!(l, 12);
        assert_eq!(c, 5);
        assert!(!warn);
        assert_eq!(msg, "error: type mismatch");
    }

    #[test]
    fn emits_valid_koka_model_json() {
        let proj = KokaProject {
            name: "demo".into(),
            version: Some("0.1.0".into()),
            main_file: Some("src/main.kk".into()),
            test_file: None,
            include_dirs: vec!["src".into()],
        };
        let json = emit_model("/repo", "/repo/koka.toml", &proj, "");
        assert!(json.contains("\"ecosystem\":\"koka\""));
        assert!(json.contains("\"language\":\"koka\""));
    }

    #[test]
    fn detects_uncaught_koka_test_exception() {
        assert_eq!(
            parse_koka_test_failure("uncaught exception: FAIL: too small message rendered").as_deref(),
            Some("uncaught exception: FAIL: too small message rendered")
        );
        assert_eq!(
            parse_koka_test_failure("FAIL: assertion mismatch").as_deref(),
            Some("FAIL: assertion mismatch")
        );
        assert!(parse_koka_test_failure("  ok: sessions count").is_none());
    }
}
