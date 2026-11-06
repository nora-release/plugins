//go:build linux

package ops

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const sharedPythonVenvPath = "/opt/nora/python/venv"

func ensurePythonEnv() (string, error) {
	pythonPath := filepath.Join(sharedPythonVenvPath, "bin", "python3")
	if _, err := os.Stat(pythonPath); err == nil {
		return pythonPath, nil
	}

	if err := os.MkdirAll(sharedPythonVenvPath, 0755); err != nil {
		return "", fmt.Errorf("create venv dir: %w", err)
	}

	output, err := exec.Command("python3", "-m", "venv", sharedPythonVenvPath).CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("create venv: %v: %s", err, output)
	}

	return pythonPath, nil
}

// hasPip checks if pip is available in the venv.
func hasPip(pythonPath string) bool {
	return exec.Command(pythonPath, "-m", "pip", "--version").Run() == nil
}

// installPip installs pip using get-pip.py.
func installPip(pythonPath string) error {
	script := `
import urllib.request, sys, tempfile, subprocess
url = "https://bootstrap.pypa.io/get-pip.py"
with tempfile.NamedTemporaryFile(mode='w', suffix='.py', delete=False) as f:
    f.write(urllib.request.urlopen(url).read().decode())
    f.flush()
    subprocess.check_call([sys.executable, f.name])
`
	output, err := exec.Command(pythonPath, "-c", script).CombinedOutput()
	if err != nil {
		return fmt.Errorf("install pip: %s", output)
	}
	return nil
}

func (s *Sandbox) preparePythonExecution(code string) (string, string, error) {
	if !s.IsMounted() {
		return "", "", fmt.Errorf("sandbox %s is not mounted", s.ID)
	}

	pythonPath, err := ensurePythonEnv()
	if err != nil {
		return "", "", fmt.Errorf("ensure python venv: %w", err)
	}

	// Ensure pip exists
	if !hasPip(pythonPath) {
		if err := installPip(pythonPath); err != nil {
			return "", "", err
		}
	}

	// Parse imports and install missing deps
	imports := parseImports(pythonPath, code)
	if len(imports) > 0 {
		if err := installMissing(pythonPath, imports); err != nil {
			return "", "", fmt.Errorf("install dependencies: %w", err)
		}
	}

	// Remote sandboxes execute against the live VM root, so keep the script
	// under the sandbox state dir to avoid collisions across environments.
	hostScriptPath := filepath.Join(s.RootDir, "/tmp/script.py")
	scriptPath := "/tmp/script.py"
	if remoteFilesystemVirtualizationDisabled() {
		hostScriptPath = filepath.Join(s.BasePath, "tmp", "script.py")
		scriptPath = hostScriptPath
	}
	if err := os.MkdirAll(filepath.Dir(hostScriptPath), 0755); err != nil {
		return "", "", fmt.Errorf("mkdir: %w", err)
	}
	if err := os.WriteFile(hostScriptPath, []byte(code), 0644); err != nil {
		return "", "", fmt.Errorf("write script: %w", err)
	}
	return pythonPath, scriptPath, nil
}

// ExecutePython executes Python code with automatic dependency management.
func (s *Sandbox) ExecutePython(ctx context.Context, code, workingDir string, env map[string]string) (stdout, stderr string, exitCode int, err error) {
	pythonPath, scriptPath, err := s.preparePythonExecution(code)
	if err != nil {
		return "", "", -1, err
	}
	return s.ExecWithContext(ctx, []string{pythonPath, scriptPath}, workingDir, env)
}

// ExecutePythonStream executes Python code with automatic dependency management and streams output.
func (s *Sandbox) ExecutePythonStream(
	ctx context.Context,
	code, workingDir string,
	env map[string]string,
	onStdout func([]byte),
	onStderr func([]byte),
	onExit func(int),
) error {
	pythonPath, scriptPath, err := s.preparePythonExecution(code)
	if err != nil {
		return err
	}
	return s.ExecStream(ctx, []string{pythonPath, scriptPath}, workingDir, env, onStdout, onStderr, onExit)
}

// parseImports uses Python ast to extract top-level imports.
func parseImports(pythonPath, code string) []string {
	script := `
import ast, sys, json
code = sys.stdin.read()
try:
    tree = ast.parse(code)
except:
    print("[]")
    sys.exit(0)
mods = set()
for node in ast.walk(tree):
    if isinstance(node, ast.Import):
        for a in node.names:
            mods.add(a.name.split('.')[0])
    elif isinstance(node, ast.ImportFrom) and node.module:
        mods.add(node.module.split('.')[0])
print(json.dumps(list(mods)))
`
	cmd := exec.Command(pythonPath, "-c", script)
	cmd.Stdin = strings.NewReader(code)
	out, err := cmd.Output()
	if err != nil {
		return nil
	}
	var imports []string
	json.Unmarshal(bytes.TrimSpace(out), &imports)
	return imports
}

// installMissing checks which imports are missing and installs them using pipreqs mapping.
func installMissing(pythonPath string, imports []string) error {
	script := `
import sys, json, subprocess, os

imports = json.loads(sys.argv[1])

# Check what's missing
missing = []
for imp in imports:
    try:
        __import__(imp)
    except ImportError:
        missing.append(imp)

if not missing:
    sys.exit(0)

# Use pipreqs to map import names to package names
try:
    from pipreqs import pipreqs
except ImportError:
    result = subprocess.run([sys.executable, "-m", "pip", "install", "-q", "pipreqs"], 
                            capture_output=True, text=True)
    if result.returncode != 0:
        print(f"Failed to install pipreqs: {result.stderr or result.stdout}", file=sys.stderr)
        sys.exit(1)
    from pipreqs import pipreqs

# Load mapping
mapping_file = os.path.join(os.path.dirname(pipreqs.__file__), "mapping")
import_to_pkg = {}
if os.path.exists(mapping_file):
    with open(mapping_file) as f:
        for line in f:
            if ":" in line:
                k, v = line.strip().split(":", 1)
                import_to_pkg[k.strip()] = v.strip()

# Map to package names
packages = [import_to_pkg.get(m, m) for m in missing]

# Install
result = subprocess.run(
    [sys.executable, "-m", "pip", "install", "-q"] + packages,
    capture_output=True, text=True
)
if result.returncode != 0:
    print(f"pip install {packages}: {result.stderr or result.stdout}", file=sys.stderr)
    sys.exit(1)
`
	importsJSON, _ := json.Marshal(imports)
	cmd := exec.Command(pythonPath, "-c", script, string(importsJSON))
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("%s", strings.TrimSpace(string(output)))
	}
	return nil
}
