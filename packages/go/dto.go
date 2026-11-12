// Package dto provides high-performance, WebAssembly-powered DTO data transformation for Go.
//
// Powered by a Rust core compiled to WebAssembly and executed via Wazero (100% pure Go, zero CGO).
package dto

import (
	"context"
	_ "embed"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"sync"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
)

//go:embed dto.wasm
var wasmBytes []byte

// Engine represents a DTO transformation engine backed by the WebAssembly runtime.
// Thread-safe and non-blocking across concurrent goroutines using an instance pool.
type Engine struct {
	runtime  wazero.Runtime
	compiled wazero.CompiledModule
	pool     sync.Pool
	ctx      context.Context
}

type instance struct {
	mod       api.Module
	alloc     api.Function
	dealloc   api.Function
	transform api.Function
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
	mod, err := e.runtime.InstantiateModule(ctx, e.compiled, wazero.NewModuleConfig())
	if err != nil {
		return nil, err
	}

	return &instance{
		mod:       mod,
		alloc:     mod.ExportedFunction("dto_alloc"),
		dealloc:   mod.ExportedFunction("dto_dealloc"),
		transform: mod.ExportedFunction("dto_transform"),
	}, nil
}

// Transform transforms input JSON data using either a Recipe or JSON Schema.
//
// schema: UTF-8 JSON string/bytes of the Recipe or JSON Schema with x-from annotations.
// data: UTF-8 JSON string/bytes of the object or array of objects to transform.
//
// Returns the transformed JSON bytes.
func (e *Engine) Transform(schema, data []byte) ([]byte, error) {
	inst := e.pool.Get().(*instance)
	defer e.pool.Put(inst)

	ctx := e.ctx

	// Allocate schema buffer in WASM memory
	schemaLen := uint64(len(schema))
	res, err := inst.alloc.Call(ctx, schemaLen)
	if err != nil {
		return nil, fmt.Errorf("dto: failed to allocate schema memory: %w", err)
	}
	schemaPtr := res[0]
	defer inst.dealloc.Call(ctx, schemaPtr, schemaLen)

	if !inst.mod.Memory().Write(uint32(schemaPtr), schema) {
		return nil, fmt.Errorf("dto: failed to write schema into wasm memory")
	}

	// Allocate data buffer in WASM memory
	dataLen := uint64(len(data))
	res, err = inst.alloc.Call(ctx, dataLen)
	if err != nil {
		return nil, fmt.Errorf("dto: failed to allocate data memory: %w", err)
	}
	dataPtr := res[0]
	defer inst.dealloc.Call(ctx, dataPtr, dataLen)

	if !inst.mod.Memory().Write(uint32(dataPtr), data) {
		return nil, fmt.Errorf("dto: failed to write data into wasm memory")
	}

	// Call dto_transform(schemaPtr, schemaLen, dataPtr, dataLen)
	res, err = inst.transform.Call(ctx, schemaPtr, schemaLen, dataPtr, dataLen)
	if err != nil {
		return nil, fmt.Errorf("dto: transform execution failed: %w", err)
	}
	resPtr := uint32(res[0])

	// Read 4-byte little-endian length prefix
	lenBytes, ok := inst.mod.Memory().Read(resPtr, 4)
	if !ok {
		return nil, fmt.Errorf("dto: failed to read result length prefix from wasm memory")
	}
	payloadLen := binary.LittleEndian.Uint32(lenBytes)

	// Read payload
	payload, ok := inst.mod.Memory().Read(resPtr+4, payloadLen)
	if !ok {
		return nil, fmt.Errorf("dto: failed to read result payload from wasm memory")
	}

	// Make a Go copy before freeing WASM memory
	result := make([]byte, payloadLen)
	copy(result, payload)

	// Free result buffer in WASM memory
	_, _ = inst.dealloc.Call(ctx, uint64(resPtr), uint64(payloadLen+4))

	// Check if the result is an error JSON
	if len(result) > 0 && result[0] == '{' {
		var errCheck struct {
			Error string `json:"error"`
		}
		if jsonErr := json.Unmarshal(result, &errCheck); jsonErr == nil && errCheck.Error != "" {
			return nil, fmt.Errorf("dto: %s", errCheck.Error)
		}
	}

	return result, nil
}

// Close releases the WebAssembly runtime resources.
func (e *Engine) Close(ctx context.Context) error {
	return e.runtime.Close(ctx)
}
