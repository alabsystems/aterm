// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `aterm_gl` import module: WebGL2 bindings for `aterm-gpu`'s GL arm.

/// One import: its name, its wasm signature, and its JS function expression.
pub(crate) struct GlImport {
    pub(crate) name: &'static str,
    pub(crate) params: &'static [u8],
    pub(crate) results: &'static [u8],
    pub(crate) body: &'static str,
}

/// The import module's name.
pub(crate) const MODULE: &str = "aterm_gl";

/// `i32`, every argument and result here: enums, names, sizes, offsets and
/// pointers into the module's memory.
const I: u8 = 0x7f;

/// Shared JS state for context and object tables and string helpers.
pub(crate) const STATE: &str = "\
const glContexts = [null];
const glContextFree = [];
const glObjects = [null];
const glOwners = [null];
const glFree = [];
let GLctx = null;
let glCurrent = null;
function glNew(obj) {
    if (obj === null || obj === undefined) return 0;
    const name = glFree.length > 0 ? glFree.pop() : glObjects.length;
    glObjects[name] = obj;
    glOwners[name] = glCurrent;
    glCurrent.objects.add(name);
    return name;
}
function glGet(name) {
    return name === 0 ? null : glObjects[name];
}
function glRelease(name, owner = glCurrent) {
    if (name !== 0 && glObjects[name] !== null && glObjects[name] !== undefined
        && glOwners[name] === owner) {
        owner.objects.delete(name);
        glObjects[name] = null;
        glOwners[name] = null;
        glFree.push(name);
    }
}
function glCString(text) {
    const bytes = new TextEncoder().encode(text);
    const ptr = wasm.aterm_alloc(bytes.length + 1, 1) >>> 0;
    const mem = getUint8ArrayMemory0();
    mem.set(bytes, ptr);
    mem[ptr + bytes.length] = 0;
    return { ptr, len: bytes.length + 1 };
}
function glString(key, text) {
    if (text === null || text === undefined) return 0;
    let allocation = glCurrent.strings.get(key);
    if (allocation === undefined) {
        allocation = glCString(String(text));
        glCurrent.strings.set(key, allocation);
    }
    return allocation.ptr;
}
function glReadCString(ptr) {
    const mem = getUint8ArrayMemory0();
    let end = ptr;
    while (mem[end] !== 0) end++;
    return decodeText(ptr, end - ptr);
}
function glView(type) {
    switch (type) {
        case 0x1406: return [getFloat32ArrayMemory0(), 2];
        case 0x1405: return [getUint32ArrayMemory0(), 2];
        case 0x140B: case 0x1403: return [getUint16ArrayMemory0(), 1];
        default: return [getUint8ArrayMemory0(), 0];
    }
}
function glExtensions() {
    return GLctx.getSupportedExtensions() || [];
}
function glWriteLog(log, bufSize, lengthPtr, infoLogPtr) {
    const bytes = new TextEncoder().encode(log);
    const n = Math.max(0, Math.min(bytes.length, bufSize - 1));
    if (bufSize > 0 && infoLogPtr !== 0) {
        const mem = getUint8ArrayMemory0();
        mem.set(bytes.subarray(0, n), infoLogPtr);
        mem[infoLogPtr + n] = 0;
    }
    if (lengthPtr !== 0) getDataViewMemory0().setInt32(lengthPtr, n, true);
}
function glGen(n, ptr, create) {
    const dv = getDataViewMemory0();
    for (let i = 0; i < n; i++) dv.setUint32(ptr + 4 * i, glNew(create()), true);
}
function glDeleteNames(n, ptr, destroy) {
    const dv = getDataViewMemory0();
    for (let i = 0; i < n; i++) {
        const name = dv.getUint32(ptr + 4 * i, true);
        if (name !== 0) {
            destroy(glGet(name), name);
            glRelease(name);
        }
    }
}
";

/// Typed array views required by [`STATE`] and the imports.
pub(crate) const VIEWS: [&str; 4] = ["Uint8Array", "Uint16Array", "Uint32Array", "Float32Array"];

/// Every import the arm may make, sorted by name.
pub(crate) const IMPORTS: &[GlImport] = &[
    GlImport {
        name: "canvas_resize",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            const c = GLctx.canvas;
            if (c.width !== arg0) c.width = arg0;
            if (c.height !== arg1) c.height = arg1;
        }",
    },
    GlImport {
        name: "context_create",
        params: &[I],
        results: &[I],
        body: "function(arg0) {
            let gl = null;
            try {
                const canvas = getObject(arg0);
                if (canvas && typeof canvas.getContext === 'function') {
                    gl = canvas.getContext('webgl2', { antialias: false });
                }
            } catch (e) {
                gl = null;
            }
            if (!gl) return 0;
            const debug = gl.getExtension('WEBGL_debug_renderer_info') !== null;
            gl.getExtension('EXT_color_buffer_float');
            const id = glContextFree.length > 0 ? glContextFree.pop() : glContexts.length;
            glContexts[id] = { gl, debug, strings: new Map(), packBuffer: 0,
                objects: new Set(), programLocations: new Map() };
            return id;
        }",
    },
    GlImport {
        name: "context_drop",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            const rec = glContexts[arg0];
            if (!rec) return;
            for (const allocation of rec.strings.values()) {
                wasm.aterm_free(allocation.ptr, allocation.len, 1);
            }
            rec.strings.clear();
            for (const name of rec.objects) glRelease(name, rec);
            rec.programLocations.clear();
            if (glCurrent === rec) {
                GLctx = null;
                glCurrent = null;
            }
            glContexts[arg0] = null;
            glContextFree.push(arg0);
        }",
    },
    GlImport {
        name: "context_lost",
        params: &[],
        results: &[I],
        body: "function() {
            return GLctx === null || GLctx.isContextLost() ? 1 : 0;
        }",
    },
    GlImport {
        name: "glActiveTexture",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.activeTexture(arg0);
        }",
    },
    GlImport {
        name: "glAttachShader",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            GLctx.attachShader(glGet(arg0), glGet(arg1));
        }",
    },
    GlImport {
        name: "glBindBuffer",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            if (arg0 === 0x88EB) glCurrent.packBuffer = arg1;
            GLctx.bindBuffer(arg0, glGet(arg1));
        }",
    },
    GlImport {
        name: "glBindBufferBase",
        params: &[I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2) {
            GLctx.bindBufferBase(arg0, arg1, glGet(arg2));
        }",
    },
    GlImport {
        name: "glBindFramebuffer",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            GLctx.bindFramebuffer(arg0, glGet(arg1));
        }",
    },
    GlImport {
        name: "glBindSampler",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            GLctx.bindSampler(arg0, glGet(arg1));
        }",
    },
    GlImport {
        name: "glBindTexture",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            GLctx.bindTexture(arg0, glGet(arg1));
        }",
    },
    GlImport {
        name: "glBindVertexArray",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.bindVertexArray(glGet(arg0));
        }",
    },
    GlImport {
        name: "glBlendEquationSeparate",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            GLctx.blendEquationSeparate(arg0, arg1);
        }",
    },
    GlImport {
        name: "glBlendFuncSeparate",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            GLctx.blendFuncSeparate(arg0, arg1, arg2, arg3);
        }",
    },
    GlImport {
        name: "glBlitFramebuffer",
        params: &[I, I, I, I, I, I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8, arg9) {
            GLctx.blitFramebuffer(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8, arg9);
        }",
    },
    GlImport {
        name: "glBufferData",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            if (arg2 === 0) {
                GLctx.bufferData(arg0, arg1, arg3);
            } else {
                GLctx.bufferData(arg0, getUint8ArrayMemory0(), arg3, arg2, arg1);
            }
        }",
    },
    GlImport {
        name: "glBufferSubData",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            GLctx.bufferSubData(arg0, arg1, getUint8ArrayMemory0(), arg3, arg2);
        }",
    },
    GlImport {
        name: "glCheckFramebufferStatus",
        params: &[I],
        results: &[I],
        body: "function(arg0) {
            return GLctx.checkFramebufferStatus(arg0);
        }",
    },
    GlImport {
        name: "glClearBufferfv",
        params: &[I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2) {
            GLctx.clearBufferfv(arg0, arg1, getFloat32ArrayMemory0(), arg2 >>> 2);
        }",
    },
    GlImport {
        name: "glColorMask",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            GLctx.colorMask(arg0 !== 0, arg1 !== 0, arg2 !== 0, arg3 !== 0);
        }",
    },
    GlImport {
        name: "glCompileShader",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.compileShader(glGet(arg0));
        }",
    },
    GlImport {
        name: "glCopyTexSubImage2D",
        params: &[I, I, I, I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7) {
            GLctx.copyTexSubImage2D(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7);
        }",
    },
    GlImport {
        name: "glCreateProgram",
        params: &[],
        results: &[I],
        body: "function() {
            return glNew(GLctx.createProgram());
        }",
    },
    GlImport {
        name: "glCreateShader",
        params: &[I],
        results: &[I],
        body: "function(arg0) {
            return glNew(GLctx.createShader(arg0));
        }",
    },
    GlImport {
        name: "glDeleteBuffers",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glDeleteNames(arg0, arg1, (b, name) => {
                if (glCurrent.packBuffer === name) glCurrent.packBuffer = 0;
                GLctx.deleteBuffer(b);
            });
        }",
    },
    GlImport {
        name: "glDeleteFramebuffers",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glDeleteNames(arg0, arg1, (f) => GLctx.deleteFramebuffer(f));
        }",
    },
    GlImport {
        name: "glDeleteProgram",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.deleteProgram(glGet(arg0));
            const locations = glCurrent.programLocations.get(arg0);
            if (locations !== undefined) {
                for (const name of locations) glRelease(name);
                glCurrent.programLocations.delete(arg0);
            }
            glRelease(arg0);
        }",
    },
    GlImport {
        name: "glDeleteSamplers",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glDeleteNames(arg0, arg1, (s) => GLctx.deleteSampler(s));
        }",
    },
    GlImport {
        name: "glDeleteShader",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.deleteShader(glGet(arg0));
            glRelease(arg0);
        }",
    },
    GlImport {
        name: "glDeleteTextures",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glDeleteNames(arg0, arg1, (t) => GLctx.deleteTexture(t));
        }",
    },
    GlImport {
        name: "glDeleteVertexArrays",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glDeleteNames(arg0, arg1, (v) => GLctx.deleteVertexArray(v));
        }",
    },
    GlImport {
        name: "glDisable",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.disable(arg0);
        }",
    },
    GlImport {
        name: "glDrawArraysInstanced",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            GLctx.drawArraysInstanced(arg0, arg1, arg2, arg3);
        }",
    },
    GlImport {
        name: "glEnable",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.enable(arg0);
        }",
    },
    GlImport {
        name: "glEnableVertexAttribArray",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.enableVertexAttribArray(arg0);
        }",
    },
    GlImport {
        name: "glFlush",
        params: &[],
        results: &[],
        body: "function() {
            GLctx.flush();
        }",
    },
    GlImport {
        name: "glFramebufferTexture2D",
        params: &[I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4) {
            GLctx.framebufferTexture2D(arg0, arg1, arg2, glGet(arg3), arg4);
        }",
    },
    GlImport {
        name: "glGenBuffers",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glGen(arg0, arg1, () => GLctx.createBuffer());
        }",
    },
    GlImport {
        name: "glGenFramebuffers",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glGen(arg0, arg1, () => GLctx.createFramebuffer());
        }",
    },
    GlImport {
        name: "glGenSamplers",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glGen(arg0, arg1, () => GLctx.createSampler());
        }",
    },
    GlImport {
        name: "glGenTextures",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glGen(arg0, arg1, () => GLctx.createTexture());
        }",
    },
    GlImport {
        name: "glGenVertexArrays",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            glGen(arg0, arg1, () => GLctx.createVertexArray());
        }",
    },
    GlImport {
        name: "glGetError",
        params: &[],
        results: &[I],
        body: "function() {
            return GLctx.getError();
        }",
    },
    GlImport {
        name: "glGetIntegerv",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            const v = GLctx.getParameter(arg0);
            const n = typeof v === 'number' ? v : (v ? 1 : 0);
            getDataViewMemory0().setInt32(arg1, n, true);
        }",
    },
    GlImport {
        name: "glGetProgramInfoLog",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            glWriteLog(GLctx.getProgramInfoLog(glGet(arg0)) || '', arg1, arg2, arg3);
        }",
    },
    GlImport {
        name: "glGetProgramiv",
        params: &[I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2) {
            const p = glGet(arg0);
            let n;
            if (arg1 === 0x8B84) {
                n = new TextEncoder().encode(GLctx.getProgramInfoLog(p) || '').length + 1;
            } else {
                const v = GLctx.getProgramParameter(p, arg1);
                n = typeof v === 'number' ? v : (v ? 1 : 0);
            }
            getDataViewMemory0().setInt32(arg2, n, true);
        }",
    },
    GlImport {
        name: "glGetShaderInfoLog",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            glWriteLog(GLctx.getShaderInfoLog(glGet(arg0)) || '', arg1, arg2, arg3);
        }",
    },
    GlImport {
        name: "glGetShaderiv",
        params: &[I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2) {
            const s = glGet(arg0);
            let n;
            if (arg1 === 0x8B84) {
                n = new TextEncoder().encode(GLctx.getShaderInfoLog(s) || '').length + 1;
            } else {
                const v = GLctx.getShaderParameter(s, arg1);
                n = typeof v === 'number' ? v : (v ? 1 : 0);
            }
            getDataViewMemory0().setInt32(arg2, n, true);
        }",
    },
    GlImport {
        name: "glGetString",
        params: &[I],
        results: &[I],
        body: "function(arg0) {
            if (arg0 === 0x1F03) return glString(arg0, glExtensions().map((e) => 'GL_' + e).join(' '));
            if (arg0 === 0x9245 || arg0 === 0x9246) {
                return glCurrent.debug ? glString(arg0, GLctx.getParameter(arg0)) : 0;
            }
            return glString(arg0, GLctx.getParameter(arg0));
        }",
    },
    GlImport {
        name: "glGetUniformBlockIndex",
        params: &[I, I],
        results: &[I],
        body: "function(arg0, arg1) {
            return GLctx.getUniformBlockIndex(glGet(arg0), glReadCString(arg1));
        }",
    },
    GlImport {
        name: "glGetUniformLocation",
        params: &[I, I],
        results: &[I],
        body: "function(arg0, arg1) {
            const loc = GLctx.getUniformLocation(glGet(arg0), glReadCString(arg1));
            if (loc === null) return -1;
            const name = glNew(loc);
            let locations = glCurrent.programLocations.get(arg0);
            if (locations === undefined) {
                locations = new Set();
                glCurrent.programLocations.set(arg0, locations);
            }
            locations.add(name);
            return name;
        }",
    },
    GlImport {
        name: "glLinkProgram",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.linkProgram(glGet(arg0));
        }",
    },
    GlImport {
        name: "glPixelStorei",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            GLctx.pixelStorei(arg0, arg1);
        }",
    },
    GlImport {
        name: "glReadPixels",
        params: &[I, I, I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
            if (glCurrent.packBuffer !== 0) {
                GLctx.readPixels(arg0, arg1, arg2, arg3, arg4, arg5, arg6);
                return;
            }
            const [view, shift] = glView(arg5);
            GLctx.readPixels(arg0, arg1, arg2, arg3, arg4, arg5, view, arg6 >>> shift);
        }",
    },
    GlImport {
        name: "glSamplerParameteri",
        params: &[I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2) {
            GLctx.samplerParameteri(glGet(arg0), arg1, arg2);
        }",
    },
    GlImport {
        name: "glScissor",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            GLctx.scissor(arg0, arg1, arg2, arg3);
        }",
    },
    GlImport {
        name: "glShaderSource",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            const dv = getDataViewMemory0();
            let source = '';
            for (let i = 0; i < arg1; i++) {
                const ptr = dv.getUint32(arg2 + 4 * i, true);
                const len = arg3 === 0 ? -1 : dv.getInt32(arg3 + 4 * i, true);
                source += len < 0 ? glReadCString(ptr) : decodeText(ptr, len);
            }
            GLctx.shaderSource(glGet(arg0), source);
        }",
    },
    GlImport {
        name: "glTexParameteri",
        params: &[I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2) {
            GLctx.texParameteri(arg0, arg1, arg2);
        }",
    },
    GlImport {
        name: "glTexStorage2D",
        params: &[I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4) {
            GLctx.texStorage2D(arg0, arg1, arg2, arg3, arg4);
        }",
    },
    GlImport {
        name: "glTexSubImage2D",
        params: &[I, I, I, I, I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8) {
            const [view, shift] = glView(arg7);
            GLctx.texSubImage2D(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7, view, arg8 >>> shift);
        }",
    },
    GlImport {
        name: "glUniform1i",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            if (arg0 >= 0) GLctx.uniform1i(glGet(arg0), arg1);
        }",
    },
    GlImport {
        name: "glUniformBlockBinding",
        params: &[I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2) {
            GLctx.uniformBlockBinding(glGet(arg0), arg1, arg2);
        }",
    },
    GlImport {
        name: "glUseProgram",
        params: &[I],
        results: &[],
        body: "function(arg0) {
            GLctx.useProgram(glGet(arg0));
        }",
    },
    GlImport {
        name: "glVertexAttribDivisor",
        params: &[I, I],
        results: &[],
        body: "function(arg0, arg1) {
            GLctx.vertexAttribDivisor(arg0, arg1);
        }",
    },
    GlImport {
        name: "glVertexAttribIPointer",
        params: &[I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4) {
            GLctx.vertexAttribIPointer(arg0, arg1, arg2, arg3, arg4);
        }",
    },
    GlImport {
        name: "glVertexAttribPointer",
        params: &[I, I, I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3, arg4, arg5) {
            GLctx.vertexAttribPointer(arg0, arg1, arg2, arg3 !== 0, arg4, arg5);
        }",
    },
    GlImport {
        name: "glViewport",
        params: &[I, I, I, I],
        results: &[],
        body: "function(arg0, arg1, arg2, arg3) {
            GLctx.viewport(arg0, arg1, arg2, arg3);
        }",
    },
    GlImport {
        name: "make_current",
        params: &[I],
        results: &[I],
        body: "function(arg0) {
            const rec = glContexts[arg0] || null;
            glCurrent = rec;
            GLctx = rec === null ? null : rec.gl;
            return rec === null ? 0 : 1;
        }",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is sorted and has no name twice, so the glue's lookup and
    /// the import object it writes are one entry per import; and every body
    /// is a function expression taking exactly its signature's arguments.
    #[test]
    fn each_import_is_listed_once_with_a_body_of_its_arity() {
        for pair in IMPORTS.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "{} / {}",
                pair[0].name,
                pair[1].name
            );
        }
        for g in IMPORTS {
            let args: Vec<String> = (0..g.params.len()).map(|i| format!("arg{i}")).collect();
            assert!(
                g.body
                    .starts_with(&format!("function({}) {{", args.join(", "))),
                "{}: {}",
                g.name,
                g.body.lines().next().unwrap_or_default()
            );
            assert!(g.results.len() <= 1, "{}", g.name);
        }
    }
}
