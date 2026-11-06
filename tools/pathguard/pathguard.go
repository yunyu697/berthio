// Package pathguard 是删除路径护栏的 Go 版本，与 scripts/lib/path-guard.ps1、
// src/safety.rs 同口径。
//
// 规则：
//   - 目标必须解析为绝对路径（存在时解开 symlink），并落在显式允许根之下；
//     Windows 下比较不区分大小写，按路径分隔符做边界判断。
//   - 拒绝盘符/文件系统根、用户主目录本身及其祖先与内部、空值、
//     未展开变量（$VAR / %VAR%）。
//   - 默认 dry-run；显式 apply=true 才删除。
//   - 删除前必须把“解析后的绝对路径 + 条目数”写入确认清单（JSONL），并打印该清单路径。
package pathguard

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"time"
)

var percentVar = regexp.MustCompile(`%[A-Za-z_][A-Za-z0-9_]*%`)

// Verdict 是校验结果。
type Verdict struct {
	Path    string `json:"path"`
	Kind    string `json:"kind"`
	Entries int    `json:"entries"`
}

// ManifestEntry 写入确认清单的一行。
type ManifestEntry struct {
	Time    string `json:"time"`
	Op      string `json:"op"`
	Path    string `json:"path"`
	Kind    string `json:"kind"`
	Entries int    `json:"entries"`
	Applied bool   `json:"applied"`
}

func fold(s string) string {
	if runtime.GOOS == "windows" {
		return strings.ToLower(s)
	}
	return s
}

func trimSep(s string) string {
	return strings.TrimRight(s, `\/`)
}

func under(root, target string) bool {
	r := fold(trimSep(root))
	t := fold(trimSep(target))
	if t == r {
		return false
	}
	return strings.HasPrefix(t, r+`\`) || strings.HasPrefix(t, r+"/")
}

func same(a, b string) bool {
	return fold(trimSep(a)) == fold(trimSep(b))
}

func isRoot(p string) bool {
	return filepath.Dir(p) == p
}

func userHomes() []string {
	var out []string
	add := func(v string) {
		if strings.TrimSpace(v) == "" {
			return
		}
		abs, err := filepath.Abs(v)
		if err != nil {
			return
		}
		if real, err := filepath.EvalSymlinks(abs); err == nil {
			abs = real
		}
		for _, e := range out {
			if same(e, abs) {
				return
			}
		}
		out = append(out, abs)
	}
	if h, err := os.UserHomeDir(); err == nil {
		add(h)
	}
	add(os.Getenv("USERPROFILE"))
	add(os.Getenv("HOME"))
	return out
}

func countEntries(p string) int {
	n := 0
	info, err := os.Lstat(p)
	if err != nil || !info.IsDir() {
		return n
	}
	_ = filepath.Walk(p, func(path string, fi os.FileInfo, err error) error {
		if err != nil || path == p {
			return nil
		}
		n++
		return nil
	})
	return n
}

// Check 只校验，不删除。成功返回规范化后的绝对路径与条目数。
func Check(path string, allowedRoots []string) (Verdict, error) {
	if strings.TrimSpace(path) == "" {
		return Verdict{}, fmt.Errorf("空值或未定义路径")
	}
	if strings.Contains(path, "$") || percentVar.MatchString(path) {
		return Verdict{}, fmt.Errorf("路径含未展开变量（$VAR / %%VAR%%）: %s", path)
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return Verdict{}, fmt.Errorf("无法解析路径 %s: %w", path, err)
	}
	abs = filepath.Clean(abs)
	if real, err := filepath.EvalSymlinks(abs); err == nil {
		abs = real
	}
	if isRoot(abs) {
		return Verdict{}, fmt.Errorf("拒绝删除盘符/文件系统根: %s", abs)
	}
	for _, h := range userHomes() {
		if same(abs, h) {
			return Verdict{}, fmt.Errorf("拒绝删除用户主目录本身: %s", abs)
		}
		if under(abs, h) {
			return Verdict{}, fmt.Errorf("拒绝删除用户主目录的祖先: %s（主目录=%s）", abs, h)
		}
		if under(h, abs) {
			return Verdict{}, fmt.Errorf("拒绝删除用户主目录之内的路径: %s", abs)
		}
	}
	if len(allowedRoots) == 0 {
		return Verdict{}, fmt.Errorf("未提供允许根")
	}
	matched := false
	for _, r := range allowedRoots {
		if strings.TrimSpace(r) == "" {
			continue
		}
		rAbs, err := filepath.Abs(r)
		if err != nil {
			continue
		}
		if real, err := filepath.EvalSymlinks(rAbs); err == nil {
			rAbs = real
		}
		rAbs = filepath.Clean(rAbs)
		if isRoot(rAbs) {
			continue
		}
		if same(abs, rAbs) {
			return Verdict{}, fmt.Errorf("拒绝删除允许根本身: %s", rAbs)
		}
		if under(rAbs, abs) {
			matched = true
			break
		}
	}
	if !matched {
		return Verdict{}, fmt.Errorf("目标不在允许根之下: %s", abs)
	}
	kind := "Missing"
	info, err := os.Lstat(abs)
	if err == nil {
		if info.IsDir() {
			kind = "Directory"
		} else {
			kind = "File"
		}
	}
	return Verdict{Path: abs, Kind: kind, Entries: countEntries(abs)}, nil
}

func writeManifest(manifest string, e ManifestEntry) error {
	if strings.TrimSpace(manifest) == "" {
		return fmt.Errorf("未提供确认清单路径")
	}
	f, err := os.OpenFile(manifest, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("写入确认清单失败 %s: %w", manifest, err)
	}
	defer f.Close()
	b, err := json.Marshal(e)
	if err != nil {
		return err
	}
	_, err = f.WriteString(string(b) + "\n")
	return err
}

// RemoveAll 递归删除；默认 dry-run，apply=true 才真正删除。
// 无论是否 apply，都会先写确认清单并打印绝对路径与条目数。
func RemoveAll(path string, allowedRoots []string, apply bool, manifest string) (Verdict, error) {
	v, err := Check(path, allowedRoots)
	if err != nil {
		return Verdict{}, err
	}
	if v.Kind == "Missing" {
		return v, nil
	}
	e := ManifestEntry{Time: time.Now().Format(time.RFC3339), Op: "remove-all", Path: v.Path, Kind: v.Kind, Entries: v.Entries, Applied: apply}
	if err := writeManifest(manifest, e); err != nil {
		return Verdict{}, err
	}
	fmt.Printf("[guard] %s remove-all %s（%d 个条目）；清单=%s\n", map[bool]string{true: "APPLY", false: "DRY-RUN"}[apply], v.Path, v.Entries, manifest)
	if apply {
		if err := os.RemoveAll(v.Path); err != nil {
			return Verdict{}, fmt.Errorf("删除目录失败 %s: %w", v.Path, err)
		}
	}
	return v, nil
}

// RemoveFile 删除单个文件；规则同 RemoveAll。
func RemoveFile(path string, allowedRoots []string, apply bool, manifest string) (Verdict, error) {
	v, err := Check(path, allowedRoots)
	if err != nil {
		return Verdict{}, err
	}
	if v.Kind == "Missing" {
		return v, nil
	}
	if v.Kind == "Directory" {
		return Verdict{}, fmt.Errorf("目标不是文件: %s", v.Path)
	}
	e := ManifestEntry{Time: time.Now().Format(time.RFC3339), Op: "remove-file", Path: v.Path, Kind: v.Kind, Entries: v.Entries, Applied: apply}
	if err := writeManifest(manifest, e); err != nil {
		return Verdict{}, err
	}
	fmt.Printf("[guard] %s remove-file %s；清单=%s\n", map[bool]string{true: "APPLY", false: "DRY-RUN"}[apply], v.Path, manifest)
	if apply {
		if err := os.Remove(v.Path); err != nil {
			return Verdict{}, fmt.Errorf("删除文件失败 %s: %w", v.Path, err)
		}
	}
	return v, nil
}
