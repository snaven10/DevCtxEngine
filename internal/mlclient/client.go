package mlclient

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/snaven10/devai/internal/config"
	"github.com/snaven10/devai/internal/runtime"
)

// StdioClient communicates with the Python ML service via JSON-RPC over stdio.
type StdioClient struct {
	cmd       *exec.Cmd
	stdin     io.WriteCloser
	stdout    *bufio.Reader
	mu        sync.Mutex
	nextID    atomic.Int64
	quiet     bool                // suppress stderr forwarding (for MCP mode)
	extraEnv  []string            // additional env vars for the ML process ("KEY=VALUE")
	stateDir  string              // state directory to pass to ML process (--state-dir)
	model     string              // embedding model key to pass to ML process (--model)
	projectCfg *config.ProjectConfig // optional project config for python resolution
}

// Option configures the client.
type Option func(*StdioClient)

// WithQuiet suppresses ML service log forwarding to stderr.
// Use this when running as MCP server to avoid polluting the MCP transport.
func WithQuiet() Option {
	return func(c *StdioClient) { c.quiet = true }
}

// WithEnv appends extra environment variables to the ML service process.
// Each entry should be in "KEY=VALUE" format. These are merged with the
// current process environment (not replacing it).
func WithEnv(env []string) Option {
	return func(c *StdioClient) { c.extraEnv = env }
}

// WithConfig provides a project configuration for Python binary resolution
// and state directory resolution. If the config has a StateDir set, it will
// be used as the default --state-dir for the ML process.
func WithConfig(cfg *config.ProjectConfig) Option {
	return func(c *StdioClient) {
		c.projectCfg = cfg
		if cfg != nil && cfg.StateDir != "" && c.stateDir == "" {
			c.stateDir = cfg.StateDir
		}
		if cfg != nil && cfg.Embeddings.Model != "" && c.model == "" {
			c.model = cfg.Embeddings.Model
		}
	}
}

// WithStateDir sets the state directory passed to the ML process via --state-dir.
// This takes precedence over the value from WithConfig.
func WithStateDir(dir string) Option {
	return func(c *StdioClient) { c.stateDir = dir }
}

type jsonRPCRequest struct {
	JSONRPC string      `json:"jsonrpc"`
	Method  string      `json:"method"`
	Params  interface{} `json:"params"`
	ID      int64       `json:"id"`
}

type jsonRPCResponse struct {
	JSONRPC string      `json:"jsonrpc"`
	Result  interface{} `json:"result,omitempty"`
	Error   *rpcError   `json:"error,omitempty"`
	ID      int64       `json:"id"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

// NewStdioClient starts the Python ML service and returns a client.
// It waits for the DEVAI_ML_READY signal before returning.
func NewStdioClient(opts ...Option) (*StdioClient, error) {
	// Apply options first so projectCfg is available for FindPython.
	client := &StdioClient{}
	for _, opt := range opts {
		opt(client)
	}
	if err := client.start(); err != nil {
		return nil, err
	}
	return client, nil
}

// start spawns a fresh Python ML process and waits for the READY signal.
// Called by NewStdioClient on first launch and by restart() on broken pipe
// after the Python process exits (e.g. via DEVAI_ML_IDLE_TIMEOUT_SEC).
func (c *StdioClient) start() error {
	pythonBin := runtime.FindPython(c.projectCfg)

	args := []string{"-m", "devai_ml.server"}
	if c.stateDir != "" {
		args = append(args, "--state-dir", c.stateDir)
	}
	if c.model != "" {
		args = append(args, "--model", c.model)
	}
	cmd := exec.Command(pythonBin, args...)

	if len(c.extraEnv) > 0 {
		cmd.Env = append(os.Environ(), c.extraEnv...)
	}

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("creating stdin pipe: %w", err)
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("creating stdout pipe: %w", err)
	}

	stderr, err := cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("creating stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("starting ML service (%s): %w", pythonBin, err)
	}

	c.cmd = cmd
	c.stdin = stdin
	c.stdout = bufio.NewReader(stdout)

	ready := make(chan error, 1)
	quiet := c.quiet
	go func() {
		scanner := bufio.NewScanner(stderr)
		for scanner.Scan() {
			line := scanner.Text()
			if !quiet {
				fmt.Fprintln(os.Stderr, "[ml] "+line)
			}
			if strings.Contains(line, "DEVAI_ML_READY") {
				ready <- nil
				for scanner.Scan() {
					if !quiet {
						fmt.Fprintln(os.Stderr, "[ml] "+scanner.Text())
					}
				}
				return
			}
		}
		ready <- fmt.Errorf("ML service exited before becoming ready")
	}()

	select {
	case err := <-ready:
		if err != nil {
			cmd.Process.Kill()
			return err
		}
	case <-time.After(120 * time.Second):
		cmd.Process.Kill()
		return fmt.Errorf("ML service startup timed out (120s) — model download may be needed")
	}

	return nil
}

// restart reaps the dead Python process and spawns a fresh one.
// Caller MUST hold c.mu. Used after a broken-pipe / EOF on Call().
func (c *StdioClient) restart() error {
	if c.cmd != nil && c.cmd.Process != nil {
		_ = c.cmd.Process.Kill()
		_ = c.cmd.Wait()
	}
	if c.stdin != nil {
		_ = c.stdin.Close()
	}
	c.cmd = nil
	c.stdin = nil
	c.stdout = nil
	return c.start()
}

// isDeadProcessErr reports whether err indicates the child process exited
// (broken pipe on write, EOF on read, or closed-pipe variants).
func isDeadProcessErr(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrClosedPipe) || errors.Is(err, syscall.EPIPE) {
		return true
	}
	msg := err.Error()
	return strings.Contains(msg, "broken pipe") || strings.Contains(msg, "file already closed")
}

// Call sends a JSON-RPC request and waits for the response.
// If the Python ML process has exited (e.g. via the idle watchdog), Call
// transparently respawns it and retries the request once.
func (c *StdioClient) Call(method string, params interface{}) (interface{}, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.callOnce(method, params, true)
}

func (c *StdioClient) callOnce(method string, params interface{}, allowRetry bool) (interface{}, error) {
	id := c.nextID.Add(1)

	req := jsonRPCRequest{
		JSONRPC: "2.0",
		Method:  method,
		Params:  params,
		ID:      id,
	}

	data, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("marshaling request: %w", err)
	}

	if _, err := c.stdin.Write(append(data, '\n')); err != nil {
		if allowRetry && isDeadProcessErr(err) {
			if rerr := c.restart(); rerr != nil {
				return nil, fmt.Errorf("ML respawn after write fail: %w (orig: %v)", rerr, err)
			}
			return c.callOnce(method, params, false)
		}
		return nil, fmt.Errorf("writing request: %w", err)
	}

	line, err := c.stdout.ReadBytes('\n')
	if err != nil {
		if allowRetry && isDeadProcessErr(err) {
			if rerr := c.restart(); rerr != nil {
				return nil, fmt.Errorf("ML respawn after read fail: %w (orig: %v)", rerr, err)
			}
			return c.callOnce(method, params, false)
		}
		return nil, fmt.Errorf("reading response: %w", err)
	}

	var resp jsonRPCResponse
	if err := json.Unmarshal(line, &resp); err != nil {
		return nil, fmt.Errorf("unmarshaling response: %w", err)
	}

	if resp.Error != nil {
		return nil, fmt.Errorf("RPC error %d: %s", resp.Error.Code, resp.Error.Message)
	}

	return resp.Result, nil
}

// PushIndex pushes local vectors for a repo+branch to the shared Qdrant store.
func (c *StdioClient) PushIndex(repo, branch string) (interface{}, error) {
	params := map[string]string{"repo": repo}
	if branch != "" {
		params["branch"] = branch
	}
	return c.Call("push_index", params)
}

// PullIndex pulls vectors for a repo+branch from the shared Qdrant store to local.
func (c *StdioClient) PullIndex(repo, branch string) (interface{}, error) {
	params := map[string]string{"repo": repo}
	if branch != "" {
		params["branch"] = branch
	}
	return c.Call("pull_index", params)
}

// SyncIndex performs bidirectional sync between local and shared for a repo+branch.
func (c *StdioClient) SyncIndex(repo, branch string) (interface{}, error) {
	params := map[string]string{"repo": repo}
	if branch != "" {
		params["branch"] = branch
	}
	return c.Call("sync_index", params)
}

// MemoryContext lists recent memories filtered by project/scope (no semantic query).
func (c *StdioClient) MemoryContext(project, scope string, limit int) (interface{}, error) {
	params := map[string]interface{}{"limit": limit}
	if project != "" {
		params["project"] = project
	}
	if scope != "" {
		params["scope"] = scope
	}
	return c.Call("memory_context", params)
}

// Recall searches memories semantically with optional metadata filters.
func (c *StdioClient) Recall(query, scope, memType, project string, limit int) (interface{}, error) {
	params := map[string]interface{}{"query": query, "limit": limit}
	if scope != "" {
		params["scope"] = scope
	}
	if memType != "" {
		params["type"] = memType
	}
	if project != "" {
		params["project"] = project
	}
	return c.Call("recall", params)
}

// Remember persists a memory. Fields map matches the Python remember handler params.
func (c *StdioClient) Remember(fields map[string]interface{}) (interface{}, error) {
	return c.Call("remember", fields)
}

// MemoriesBySymbol returns memories that reference a specific code symbol.
func (c *StdioClient) MemoriesBySymbol(symbol, repo, branch string, limit int) (interface{}, error) {
	p := map[string]interface{}{"symbol": symbol, "limit": limit}
	if repo != "" {
		p["repo"] = repo
	}
	if branch != "" {
		p["branch"] = branch
	}
	return c.Call("memories_by_symbol", p)
}

// MemoriesByFile returns memories that reference a file path.
func (c *StdioClient) MemoriesByFile(file string, limit int) (interface{}, error) {
	return c.Call("memories_by_file", map[string]interface{}{"file": file, "limit": limit})
}

// MemoryRefs returns the junction rows (symbol, file, source) for one memory.
func (c *StdioClient) MemoryRefs(memoryID int) (interface{}, error) {
	return c.Call("memory_refs", map[string]interface{}{"id": memoryID})
}

// ImpactAnalysis traces upstream callers + downstream callees of a symbol.
// depth caps the BFS (1=direct only). kind = "calls" | "imports" | "" (any).
func (c *StdioClient) ImpactAnalysis(symbol, repo, branch string, depth int, kind string) (interface{}, error) {
	return c.Call("impact_analysis", map[string]interface{}{
		"symbol": symbol, "repo": repo, "branch": branch,
		"depth": depth, "kind": kind,
	})
}

// IndexRepo triggers a (re-)index of the repo at the given on-disk path.
// branch="" means "current". incremental=false forces a full reindex.
func (c *StdioClient) IndexRepo(repoPath, branch string, incremental bool) (interface{}, error) {
	p := map[string]interface{}{
		"repo_path":   repoPath,
		"incremental": incremental,
	}
	if branch != "" {
		p["branch"] = branch
	}
	return c.Call("index_repo", p)
}

// FTSRebuild populates (or rebuilds) the graph_symbols_fts index.
func (c *StdioClient) FTSRebuild(force bool) (interface{}, error) {
	return c.Call("fts_rebuild", map[string]interface{}{"force": force})
}

// ExtractQuarkusRoutes scans the indexed .java files of (repo, branch) and
// persists Quarkus/JAX-RS REST routes. sourceRoot is the absolute on-disk
// path of the repo; pass "" to let the Python side auto-detect.
func (c *StdioClient) ExtractQuarkusRoutes(repo, branch, sourceRoot string) (interface{}, error) {
	p := map[string]interface{}{"repo": repo, "branch": branch}
	if sourceRoot != "" {
		p["source_root"] = sourceRoot
	}
	return c.Call("extract_quarkus_routes", p)
}

// ExtractRoutes is the generic dispatcher. framework ∈
// quarkus|spring|fastapi|flask|express|nestjs|angular.
func (c *StdioClient) ExtractRoutes(framework, repo, branch, sourceRoot string) (interface{}, error) {
	p := map[string]interface{}{"framework": framework, "repo": repo, "branch": branch}
	if sourceRoot != "" {
		p["source_root"] = sourceRoot
	}
	return c.Call("extract_routes", p)
}

// SearchRoutes finds routes matching a path substring + optional filters.
func (c *StdioClient) SearchRoutes(q, framework, httpMethod, repo, branch string, limit int) (interface{}, error) {
	p := map[string]interface{}{"limit": limit}
	if q != "" {
		p["q"] = q
	}
	if framework != "" {
		p["framework"] = framework
	}
	if httpMethod != "" {
		p["http_method"] = httpMethod
	}
	if repo != "" {
		p["repo"] = repo
	}
	if branch != "" {
		p["branch"] = branch
	}
	return c.Call("search_routes", p)
}

// RoutesForHandler returns the route(s) served by a given Java handler symbol.
func (c *StdioClient) RoutesForHandler(handlerSymbol string) (interface{}, error) {
	return c.Call("routes_for_handler", map[string]interface{}{"handler_symbol": handlerSymbol})
}

// SymbolMemoryCounts returns {symbol: count} for the heatmap overlay.
func (c *StdioClient) SymbolMemoryCounts(repo, branch string) (interface{}, error) {
	p := map[string]interface{}{}
	if repo != "" {
		p["repo"] = repo
	}
	if branch != "" {
		p["branch"] = branch
	}
	return c.Call("symbol_memory_counts", p)
}

// BackfillSymbolRefs re-extracts symbol references for every existing memory.
func (c *StdioClient) BackfillSymbolRefs() (interface{}, error) {
	return c.Call("backfill_symbol_refs", map[string]interface{}{})
}

// BackfillVectorLinks bridges unlinked memories to code via vector similarity.
// onlyUnlinked: when true, only processes memories that have no junction rows yet.
func (c *StdioClient) BackfillVectorLinks(topK int, onlyUnlinked bool) (interface{}, error) {
	return c.Call("backfill_vector_links", map[string]interface{}{
		"top_k": topK, "only_unlinked": onlyUnlinked,
	})
}

// Close stops the Python ML service.
func (c *StdioClient) Close() error {
	c.stdin.Close()
	return c.cmd.Wait()
}
