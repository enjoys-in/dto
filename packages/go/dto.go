// Package dto provides WebAssembly-powered DTO data transformation for Go.
//
// Powered by a Rust core compiled to WebAssembly and executed via Wazero (100% pure Go, zero CGO).
package dto

import (
	"context"
	_ "embed"
	"encoding/binary"
	"errors"
	"fmt"
	"strconv"
	"sync"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
)

//go:embed dto.wasm
var wasmBytes []byte

const statusOK = 0

// Engine represents a DTO transformation engine backed by the WebAssembly runtime.
// Transform is safe for concurrent use across goroutines (it uses an instance pool).
type Engine struct {
	runtime  wazero.Runtime
	compiled wazero.CompiledModule
	pool     sync.Pool
	ctx      context.Context
}

type instance struct {
	mod             api.Module
	alloc           api.Function
	dealloc         api.Function
	engineNew       api.Function
	engineTransform api.Function
	engineFree      api.Function
}

// New creates and compiles a new DTO Engine instance using embedded WebAssembly bytes.
func New(ctx context.Context) (*Engine, error) {
	return NewWithBytes(ctx, wasmBytes)
}

// NewWithBytes creates an engine from custom WebAssembly binary bytes.
func NewWithBytes(ctx context.Context, code []byte) (*Engine, error) {
	r := wazero.NewRuntime(ctx)

	compiled, err := r.CompileModule(ctx, code)
	if err != nil {
		_ = r.Close(ctx)
		return nil, fmt.Errorf("dto: failed to compile wasm module: %w", err)
	}

	e := &Engine{
		runtime:  r,
		compiled: compiled,
		ctx:      ctx,
	}

	e.pool = sync.Pool{
		New: func() any {
			inst, err := e.instantiate(ctx)
			if err != nil {
				panic(fmt.Sprintf("dto: failed to instantiate module: %v", err))
			}
			return inst
		},
	}

	return e, nil
}

func (e *Engine) instantiate(ctx context.Context) (*instance, error) {
	// An empty name lets the runtime hold several instances of the module.
	mod, err := e.runtime.InstantiateModule(ctx, e.compiled, wazero.NewModuleConfig().WithName(""))
	if err != nil {
		return nil, err
	}

	return &instance{
		mod:             mod,
		alloc:           mod.ExportedFunction("dto_alloc"),
		dealloc:         mod.ExportedFunction("dto_dealloc"),
		engineNew:       mod.ExportedFunction("dto_engine_new"),
		engineTransform: mod.ExportedFunction("dto_engine_transform"),
		engineFree:      mod.ExportedFunction("dto_engine_free"),
	}, nil
}

// Transform transforms input JSON data using a Recipe, JSON Schema or pipeline (one-shot).
//
// schema: UTF-8 JSON bytes of the Recipe, JSON Schema or pipeline.
// data: UTF-8 JSON bytes of the object or array of objects to transform.
//
// Returns the transformed JSON bytes. To transform many inputs with the same
// schema, use Compile instead so the schema is parsed only once.
func (e *Engine) Transform(schema, data []byte) ([]byte, error) {
	inst := e.pool.Get().(*instance)
	defer e.pool.Put(inst)

	handle, err := inst.compile(e.ctx, schema)
	if err != nil {
		return nil, err
	}
	defer inst.free(e.ctx, handle)

	return inst.transform(e.ctx, handle, data)
}

// Compiled is a Recipe, JSON Schema or pipeline compiled once by Engine.Compile.
// It owns a dedicated WebAssembly instance. It is safe for concurrent use, but
// calls are serialized; compile once per goroutine for parallelism.
// Call Close when done.
type Compiled struct {
	mu     sync.Mutex
	inst   *instance
	handle uint32
	ctx    context.Context
	closed bool
}

// ErrClosed is returned when a Compiled transform is used after Close.
var ErrClosed = errors.New("dto: compiled transform is closed")

// Compile parses and validates schema once. Configuration errors (unknown
// functions, invalid paths, ...) are returned here.
func (e *Engine) Compile(schema []byte) (*Compiled, error) {
	inst, err := e.instantiate(e.ctx)
	if err != nil {
		return nil, fmt.Errorf("dto: failed to instantiate module: %w", err)
	}
	handle, err := inst.compile(e.ctx, schema)
	if err != nil {
		_ = inst.mod.Close(e.ctx)
		return nil, err
	}
	return &Compiled{inst: inst, handle: handle, ctx: e.ctx}, nil
}

// Transform transforms data (an object or an array of objects) and returns the output JSON.
func (c *Compiled) Transform(data []byte) ([]byte, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil, ErrClosed
	}
	return c.inst.transform(c.ctx, c.handle, data)
}

// Close releases the compiled transform and its WebAssembly instance. Safe to call twice.
func (c *Compiled) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil
	}
	c.closed = true
	return c.inst.mod.Close(c.ctx)
}

// Close releases the WebAssembly runtime resources.
func (e *Engine) Close(ctx context.Context) error {
	return e.runtime.Close(ctx)
}

// ── instance helpers ────────────────────────────────────────────────────

// withBytes copies b into WASM memory for the duration of fn.
func (inst *instance) withBytes(ctx context.Context, b []byte, fn func(ptr, size uint64) ([]byte, error)) ([]byte, error) {
	size := uint64(len(b))
	res, err := inst.alloc.Call(ctx, size)
	if err != nil {
		return nil, fmt.Errorf("dto: failed to allocate wasm memory: %w", err)
	}
	ptr := res[0]
	defer inst.dealloc.Call(ctx, ptr, size)

	if !inst.mod.Memory().Write(uint32(ptr), b) {
		return nil, fmt.Errorf("dto: failed to write into wasm memory")
	}
	return fn(ptr, size)
}

// readStatus reads and frees a [u32 len][u8 status][payload] result buffer.
func (inst *instance) readStatus(ctx context.Context, resPtr uint32) ([]byte, error) {
	header, ok := inst.mod.Memory().Read(resPtr, 5)
	if !ok {
		return nil, fmt.Errorf("dto: failed to read result header from wasm memory")
	}
	size := binary.LittleEndian.Uint32(header[:4])
	status := header[4]

	payload, ok := inst.mod.Memory().Read(resPtr+5, size)
	if !ok {
		return nil, fmt.Errorf("dto: failed to read result payload from wasm memory")
	}
	// Copy before freeing WASM memory.
	result := make([]byte, size)
	copy(result, payload)
	_, _ = inst.dealloc.Call(ctx, uint64(resPtr), uint64(size)+5)

	if status != statusOK {
		return nil, fmt.Errorf("dto: %s", result)
	}
	return result, nil
}

func (inst *instance) compile(ctx context.Context, schema []byte) (uint32, error) {
	out, err := inst.withBytes(ctx, schema, func(ptr, size uint64) ([]byte, error) {
		res, err := inst.engineNew.Call(ctx, ptr, size)
		if err != nil {
			return nil, fmt.Errorf("dto: compile execution failed: %w", err)
		}
		return inst.readStatus(ctx, uint32(res[0]))
	})
	if err != nil {
		return 0, err
	}
	handle, err := strconv.ParseUint(string(out), 10, 32)
	if err != nil {
		return 0, fmt.Errorf("dto: invalid engine handle %q: %w", out, err)
	}
	return uint32(handle), nil
}

func (inst *instance) transform(ctx context.Context, handle uint32, data []byte) ([]byte, error) {
	return inst.withBytes(ctx, data, func(ptr, size uint64) ([]byte, error) {
		res, err := inst.engineTransform.Call(ctx, uint64(handle), ptr, size)
		if err != nil {
			return nil, fmt.Errorf("dto: transform execution failed: %w", err)
		}
		return inst.readStatus(ctx, uint32(res[0]))
	})
}

func (inst *instance) free(ctx context.Context, handle uint32) {
	_, _ = inst.engineFree.Call(ctx, uint64(handle))
}
