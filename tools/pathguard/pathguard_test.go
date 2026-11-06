package pathguard

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func testRoot(t *testing.T) string {
	t.Helper()
	if r := os.Getenv("BERTHIO_TEST_ROOT"); r != "" {
		if err := os.MkdirAll(r, 0o755); err != nil {
			t.Fatalf("创建测试根失败: %v", err)
		}
		return r
	}
	if runtime.GOOS == "windows" {
		r := filepath.Join("D:", "berthio-pathguard-test")
		if err := os.MkdirAll(r, 0o755); err != nil {
			t.Fatalf("创建 D: 测试根失败: %v", err)
		}
		return r
	}
	return t.TempDir()
}

func mustReject(t *testing.T, path string, roots []string) {
	t.Helper()
	if _, err := Check(path, roots); err == nil {
		t.Fatalf("应拒绝但放行: %q", path)
	}
}

func TestCheckRejectsRequiredInputs(t *testing.T) {
	root := testRoot(t)
	roots := []string{root}

	mustReject(t, "", roots)
	mustReject(t, "C:\\Users\\Tyler Zhang", roots)
	mustReject(t, "C:\\", roots)
	mustReject(t, "$env:USERPROFILE", roots)
	mustReject(t, "..\\..\\Users\\Tyler Zhang", roots)
	mustReject(t, "%TEMP%", roots)
	mustReject(t, "arbitrary-$VAR", roots)

	if home, err := os.UserHomeDir(); err == nil {
		mustReject(t, home, roots)
		if parent := filepath.Dir(home); parent != home {
			mustReject(t, parent, roots)
		}
		mustReject(t, filepath.Join(home, "docs"), roots)
	}
}

func TestRemoveAllDryRunThenApply(t *testing.T) {
	root := testRoot(t)
	target := filepath.Join(root, "go-guard-"+strings.ReplaceAll(t.Name(), "/", "_"))
	_ = os.RemoveAll(target)
	if err := os.MkdirAll(filepath.Join(target, "sub"), 0o755); err != nil {
		t.Fatalf("创建目标失败: %v", err)
	}
	if err := os.WriteFile(filepath.Join(target, "sub", "a.txt"), []byte("x"), 0o644); err != nil {
		t.Fatalf("写文件失败: %v", err)
	}
	manifest := filepath.Join(root, "manifest.jsonl")
	_ = os.Remove(manifest)

	v, err := Check(target, []string{root})
	if err != nil {
		t.Fatalf("临时目录应放行: %v", err)
	}
	if v.Kind != "Directory" || v.Entries < 2 {
		t.Fatalf("条目统计异常: %+v", v)
	}

	if _, err := RemoveAll(target, []string{root}, false, manifest); err != nil {
		t.Fatalf("dry-run 失败: %v", err)
	}
	if _, err := os.Stat(target); err != nil {
		t.Fatalf("dry-run 不应删除目标: %v", err)
	}
	if _, err := os.Stat(manifest); err != nil {
		t.Fatalf("确认清单未落盘: %v", err)
	}

	if _, err := RemoveAll(target, []string{root}, true, manifest); err != nil {
		t.Fatalf("apply 失败: %v", err)
	}
	if _, err := os.Stat(target); !os.IsNotExist(err) {
		t.Fatalf("apply 应删除目标: %v", err)
	}
	data, err := os.ReadFile(manifest)
	if err != nil {
		t.Fatalf("读清单失败: %v", err)
	}
	if lines := strings.Count(strings.TrimSpace(string(data)), "\n") + 1; lines != 2 {
		t.Fatalf("清单应有 2 条记录，实际 %d", lines)
	}
}

func TestRemoveRefusesRootItself(t *testing.T) {
	root := testRoot(t)
	if _, err := RemoveAll(root, []string{root}, false, filepath.Join(root, "m.jsonl")); err == nil {
		t.Fatalf("删除允许根本身应被拒绝: %s", root)
	}
}
