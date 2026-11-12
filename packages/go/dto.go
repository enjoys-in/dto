// Package dto provides WebAssembly-powered DTO data transformation for Go.
//
// Powered by a Rust core compiled to WebAssembly and executed via Wazero (100% pure Go, zero CGO).
package dto

import (
	"container/list"
	"context"
	_ "embed"
	"encoding/binary"
	"errors"
	"fmt"
	"runtime"
	"strconv"
	"sync"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
)

//go:embed dto.wasm
var wasmBytes []byte

const (
	statusOK = 0

	// MaxInstanceMemory is the WebAssembly memory size above which an instance
	// is closed after use instead of being reused. WASM memory never shrinks,
	// so one huge input would otherwise pin that memory for the engine's lifetime.
	MaxInstanceMemory = 64 << 20

	// schemaCacheSize is how many compiled schemas each pooled instance keeps
	// for Engine.Transform (least recently used are freed first).
	schemaCacheSize = 64
)

// ErrClosed is returned when an Engine or Compiled transform is used after Close.
var ErrClosed = errors.New("dto: closed")

// Engine is a DTO transformation engine backed by a pool of WebAssembly instances.
//
// Transform is safe for concurrent use. At most GOMAXPROCS instances exist at
// once; extra callers wait for a free instance. Instances that trap, or whose
// memory grew beyond MaxInstanceMemory, are closed and replaced.
type Engine struct {
	runtime  wazero.Runtime
	compiled wazero.CompiledModule
	ctx      context.Context

	idle  chan *instance // instances ready for reuse
	slots chan struct{}  // one token per live pooled instance

	mu     sync.RWMutex
	closed bool
}

type instance struct {
	mod             api.Module
	alloc           api.Function
	dealloc         api.Function
	engineNew       api.Function
	engineTransform api.Function
	engineFree      api.Function
	engineCount     api.Function

	// broken is set when a call into WASM failed (trap): the instance's memory
	// may be inconsistent, so it must not be reused.
	broken bool

	// LRU cache of compiled schemas: schema text -> handle.
	cache map[string]*list.Element
	lru   *list.List
}

type cacheEntry struct {
	key    string
	handle uint32
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

	size := runtime.GOMAXPROCS(0)
	return &Engine{
		runtime:  r,
		compiled: compiled,
		ctx:      ctx,
		idle:     make(chan *instance, size),
		slots:    make(chan struct{}, size),
	}, nil
}

func (e *Engine) instantiate() (*instance, error) {
	// An empty name lets the runtime hold several instances of the module.
	mod, err := e.runtime.InstantiateModule(e.ctx, e.compiled, wazero.NewModuleConfig().WithName(""))
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
		engineCount:     mod.ExportedFunction("dto_engine_count"),
		cache:           make(map[string]*list.Element),
		lru:             list.New(),
	}, nil
}

// acquire returns an idle instance, creates one if under the limit, or waits.
func (e *Engine) acquire() (*instance, error) {
	select {
	case inst := <-e.idle:
		return inst, nil
	default:
	}
	select {
	case inst := <-e.idle:
		return inst, nil
	case e.slots <- struct{}{}:
		inst, err := e.instantiate()
		if err != nil {
			<-e.slots
			return nil, fmt.Errorf("dto: failed to instantiate module: %w", err)
		}
		return inst, nil
	}
}

// release returns an instance to the pool, or closes it if it is unusable or too large.
func (e *Engine) release(inst *instance) {
	if inst.broken || inst.memorySize() > MaxInstanceMemory {
		_ = inst.mod.Close(e.ctx)
		<-e.slots
		return
	}
	e.idle <- inst // never blocks: idle capacity equals the number of slots
}

// Transform transforms input JSON data using a Recipe, JSON Schema or pipeline.
//
// schema: UTF-8 JSON bytes of the Recipe, JSON Schema or pipeline.
// data: UTF-8 JSON bytes of the object or array of objects to transform.
//
// Compiled schemas are cached per instance (up to 64, least recently used
// evicted), so repeated calls with the same schema do not re-parse it.
func (e *Engine) Transform(schema, data []byte) ([]byte, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if e.closed {
		return nil, ErrClosed
	}

	inst, err := e.acquire()
	if err != nil {
		return nil, err
	}
	defer e.release(inst)

	handle, err := inst.cachedHandle(e.ctx, schema)
	if err != nil {
		return nil, err
	}
	return inst.transform(e.ctx, handle, data)
}

// Close releases the WebAssembly runtime and every instance. Calls made after
// Close return ErrClosed; Close waits for in-flight Transform calls to finish.
func (e *Engine) Close(ctx context.Context) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.closed {
		return nil
	}
	e.closed = true
	return e.runtime.Close(ctx)
}

// Compiled is a Recipe, JSON Schema or pipeline compiled once by Engine.Compile.
// It owns a dedicated WebAssembly instance. It is safe for concurrent use, but
// calls are serialized; compile once per goroutine for parallelism.
//
// Call Close when done. If a Compiled is garbage-collected without Close, a
// finalizer releases its instance.
type Compiled struct {
	mu     sync.Mutex
	engine *Engine
	schema []byte
	inst   *instance // nil until first use after a recycle
	handle uint32
	closed bool
}

// Compile parses and validates schema once. Configuration errors (unknown
// functions, invalid paths, ...) are returned here.
func (e *Engine) Compile(schema []byte) (*Compiled, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if e.closed {
		return nil, ErrClosed
	}
	c := &Compiled{engine: e, schema: append([]byte(nil), schema...)}
	if err := c.open(); err != nil {
		return nil, err
	}
	runtime.SetFinalizer(c, (*Compiled).Close)
	return c, nil
}

// open creates the dedicated instance and compiles the schema in it.
func (c *Compiled) open() error {
	inst, err := c.engine.instantiate()
	if err != nil {
		return fmt.Errorf("dto: failed to instantiate module: %w", err)
	}
	handle, err := inst.compile(c.engine.ctx, c.schema)
	if err != nil {
		_ = inst.mod.Close(c.engine.ctx)
		return err
	}
	c.inst, c.handle = inst, handle
	return nil
}

// Transform transforms data (an object or an array of objects) and returns the output JSON.
func (c *Compiled) Transform(data []byte) ([]byte, error) {
	c.engine.mu.RLock()
	defer c.engine.mu.RUnlock()
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed || c.engine.closed {
		return nil, ErrClosed
	}
	if c.inst == nil {
		if err := c.open(); err != nil {
			return nil, err
		}
	}

	out, err := c.inst.transform(c.engine.ctx, c.handle, data)
	// Replace an instance that trapped or grew too large; it is recreated on next use.
	if c.inst.broken || c.inst.memorySize() > MaxInstanceMemory {
		_ = c.inst.mod.Close(c.engine.ctx)
		c.inst = nil
	}
	return out, err
}

// Close releases the compiled transform and its WebAssembly instance. Safe to call twice.
func (c *Compiled) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil
	}
	c.closed = true
	runtime.SetFinalizer(c, nil)
	if c.inst == nil {
		return nil
	}
	err := c.inst.mod.Close(c.engine.ctx)
	c.inst = nil
	return err
}

// ── instance helpers ────────────────────────────────────────────────────

func (inst *instance) memorySize() uint32 {
	return inst.mod.Memory().Size()
}

// call invokes a WASM export and marks the instance broken if it traps.
func (inst *instance) call(ctx context.Context, fn api.Function, params ...uint64) ([]uint64, error) {
	res, err := fn.Call(ctx, params...)
	if err != nil {
		inst.broken = true
		return nil, fmt.Errorf("dto: webassembly call failed: %w", err)
	}
	return res, nil
}

// withBytes copies b into WASM memory for the duration of fn.
func (inst *instance) withBytes(ctx context.Context, b []byte, fn func(ptr, size uint64) ([]byte, error)) ([]byte, error) {
	size := uint64(len(b))
	res, err := inst.call(ctx, inst.alloc, size)
	if err != nil {
		return nil, err
	}
	ptr := res[0]
	defer func() {
		if !inst.broken {
			_, _ = inst.call(ctx, inst.dealloc, ptr, size)
		}
	}()

	if !inst.mod.Memory().Write(uint32(ptr), b) {
		return nil, fmt.Errorf("dto: failed to write into wasm memory")
	}
	return fn(ptr, size)
}

// readStatus reads and frees a [u32 len][u8 status][payload] result buffer.
func (inst *instance) readStatus(ctx context.Context, resPtr uint32) ([]byte, error) {
	header, ok := inst.mod.Memory().Read(resPtr, 5)
	if !ok {
		inst.broken = true
		return nil, fmt.Errorf("dto: failed to read result header from wasm memory")
	}
	size := binary.LittleEndian.Uint32(header[:4])
	status := header[4]

	payload, ok := inst.mod.Memory().Read(resPtr+5, size)
	if !ok {
		inst.broken = true
		return nil, fmt.Errorf("dto: failed to read result payload from wasm memory")
	}
	// Copy before freeing WASM memory.
	result := make([]byte, size)
	copy(result, payload)
	if _, err := inst.call(ctx, inst.dealloc, uint64(resPtr), uint64(size)+5); err != nil {
		return nil, err
	}

	if status != statusOK {
		return nil, fmt.Errorf("dto: %s", result)
	}
	return result, nil
}

func (inst *instance) compile(ctx context.Context, schema []byte) (uint32, error) {
	out, err := inst.withBytes(ctx, schema, func(ptr, size uint64) ([]byte, error) {
		res, err := inst.call(ctx, inst.engineNew, ptr, size)
		if err != nil {
			return nil, err
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

// cachedHandle returns a compiled handle for schema, compiling and caching it on a miss.
func (inst *instance) cachedHandle(ctx context.Context, schema []byte) (uint32, error) {
	key := string(schema)
	if el, ok := inst.cache[key]; ok {
		inst.lru.MoveToFront(el)
		return el.Value.(*cacheEntry).handle, nil
	}
	handle, err := inst.compile(ctx, schema)
	if err != nil {
		return 0, err
	}
	inst.cache[key] = inst.lru.PushFront(&cacheEntry{key: key, handle: handle})
	if inst.lru.Len() > schemaCacheSize {
		oldest := inst.lru.Back()
		entry := inst.lru.Remove(oldest).(*cacheEntry)
		delete(inst.cache, entry.key)
		inst.free(ctx, entry.handle)
	}
	return handle, nil
}

func (inst *instance) transform(ctx context.Context, handle uint32, data []byte) ([]byte, error) {
	return inst.withBytes(ctx, data, func(ptr, size uint64) ([]byte, error) {
		res, err := inst.call(ctx, inst.engineTransform, uint64(handle), ptr, size)
		if err != nil {
			return nil, err
		}
		return inst.readStatus(ctx, uint32(res[0]))
	})
}

func (inst *instance) free(ctx context.Context, handle uint32) {
	_, _ = inst.call(ctx, inst.engineFree, uint64(handle))
}

// liveHandles reports how many compiled engines exist in the instance (tests).
func (inst *instance) liveHandles(ctx context.Context) uint32 {
	res, err := inst.call(ctx, inst.engineCount)
	if err != nil {
		return 0
	}
	return uint32(res[0])
}
