//! 删除路径护栏（Rust 侧，等价于 scripts/lib/path-guard.ps1）。
//!
//! 规则：
//!   - 目标必须解析为绝对路径（存在时用 canonicalize，会解开 symlink/junction），
//!     并落在显式允许根之下；Windows 比较不区分大小写，按路径分隔符做边界判断。
//!   - 拒绝文件系统/驱动根、拒绝用户主目录及其祖先、拒绝空值、拒绝含未展开变量的路径。
//!   - 每次删除前打印绝对路径与条目数，供日志人工核对。
//!   - 调用方必须显式调用 `remove_dir_all` / `remove_file` 才会真正删除；
//!     `check_delete` 只做校验，可用于 dry-run。
//!
//! 说明：这里不拒绝“用户主目录之内”的路径（例如 Windows 的 %TEMP% 在 AppData 下），
//! 否则 `--demo` 无法在 Windows 上清理自己的临时目录；但仍要求显式允许根，
//! 因此 `TEMP` 被误设成用户主目录本身时，目标等于主目录，会被拒绝。

use std::path::{Component, Path, PathBuf};

fn as_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(windows)]
fn fold(s: &str) -> String {
    s.to_lowercase()
}

#[cfg(not(windows))]
fn fold(s: &str) -> String {
    s.to_string()
}

fn lexical_abs(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical_abs(path))
}

fn user_homes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                let p = resolved(Path::new(v.trim()));
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn has_unexpanded_variable(s: &str) -> bool {
    if s.contains('$') {
        return true;
    }
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '%' {
            let mut j = i + 1;
            if j < bytes.len() && (bytes[j].is_ascii_alphabetic() || bytes[j] == '_') {
                j += 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == '_') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == '%' {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn trim_sep(mut s: String) -> String {
    while s.ends_with('\\') || s.ends_with('/') {
        s.pop();
    }
    s
}

fn under(root: &Path, target: &Path) -> bool {
    let r = fold(&trim_sep(as_string(root)));
    let t = fold(&as_string(target));
    if t == r {
        return false;
    }
    t.starts_with(&format!("{r}\\")) || t.starts_with(&format!("{r}/"))
}

fn same(left: &Path, right: &Path) -> bool {
    fold(&trim_sep(as_string(left))) == fold(&trim_sep(as_string(right)))
}

/// 只校验，不删除。成功返回规范化后的绝对路径。
pub fn check_delete(path: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("空路径".into());
    }
    let raw = as_string(path);
    if has_unexpanded_variable(&raw) {
        return Err("路径含未展开变量（$VAR / %VAR%）".into());
    }
    let target = resolved(path);
    if target.parent().is_none() {
        return Err(format!("拒绝删除文件系统/驱动根: {}", target.display()));
    }
    for home in user_homes() {
        if same(&target, &home) {
            return Err(format!("拒绝删除用户主目录: {}", target.display()));
        }
        if home.starts_with(&target) {
            return Err(format!(
                "拒绝删除用户主目录的祖先: {}（主目录={}）",
                target.display(),
                home.display()
            ));
        }
    }
    if allowed_roots.is_empty() {
        return Err("未提供允许根".into());
    }
    let mut matched = false;
    for root in allowed_roots {
        if root.as_os_str().is_empty() {
            continue;
        }
        let r = resolved(root);
        if r.parent().is_none() {
            continue;
        }
        if same(&target, &r) {
            return Err(format!("拒绝删除允许根本身: {}", r.display()));
        }
        if under(&r, &target) {
            matched = true;
            break;
        }
    }
    if !matched {
        return Err(format!("目标不在允许根之下: {}", target.display()));
    }
    Ok(target)
}

fn count_entries(path: &Path) -> u64 {
    let mut n = 0u64;
    let Ok(meta) = std::fs::metadata(path) else {
        return n;
    };
    if meta.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                n += 1;
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    n += count_entries(&e.path());
                }
            }
        }
    }
    n
}

/// 递归删除。默认不做任何事？不：调用本函数即表示调用方显式选择“执行”，
/// 但仍必须先通过 `check_delete`；函数会打印绝对路径与条目数。
pub fn remove_dir_all(path: &Path, allowed_roots: &[PathBuf]) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let target = check_delete(path, allowed_roots)?;
    let entries = count_entries(&target);
    eprintln!(
        "[safety] remove-dir {}（{entries} 个条目）",
        target.display()
    );
    std::fs::remove_dir_all(&target)
        .map_err(|e| format!("删除目录失败 {}: {e}", target.display()))
}

/// 删除单个文件；同样先过护栏并打印绝对路径。
pub fn remove_file(path: &Path, allowed_roots: &[PathBuf]) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let target = check_delete(path, allowed_roots)?;
    eprintln!("[safety] remove-file {}", target.display());
    std::fs::remove_file(&target).map_err(|e| format!("删除文件失败 {}: {e}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_child(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "berthio-safety-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ))
    }

    #[test]
    fn rejects_empty_root_home_and_variables() {
        let root = std::env::temp_dir();
        assert!(check_delete(Path::new(""), &[root.clone()]).is_err());
        assert!(check_delete(Path::new("$HOME/x"), &[root.clone()]).is_err());
        assert!(check_delete(Path::new("%TEMP%/x"), &[root.clone()]).is_err());

        #[cfg(unix)]
        assert!(check_delete(Path::new("/"), &[root.clone()]).is_err());

        let homes = user_homes();
        for home in homes {
            assert!(check_delete(&home, &[root.clone()]).is_err());
            if let Some(parent) = home.parent() {
                assert!(check_delete(parent, &[root.clone()]).is_err());
            }
        }
    }

    #[test]
    fn allows_and_removes_temp_child() {
        let root = std::env::temp_dir();
        let dir = temp_child("ok");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), b"x").unwrap();
        let target = check_delete(&dir, &[root.clone()]).unwrap();
        assert!(target.ends_with(dir.file_name().unwrap()));
        assert!(check_delete(&root, &[root.clone()]).is_err());
        remove_dir_all(&dir, &[root]).unwrap();
        assert!(!dir.exists());
    }
}
