//! The few parts of llama.cpp a decision model needs, called straight
//! through `llama-cpp-sys-2`: load a GGUF, read its metadata, tokenize, and
//! read the logits at the last token of one prompt. Feature `decisions`.
//!
//! # Why not `llama-cpp-2`
//!
//! The safe bindings switch on the sys crate's `common` feature whatever
//! their own features say, which links llama.cpp's `common` library. One of
//! its files builds a `std::regex` at load time, with this machine's C++
//! headers; sherpa-onnx's prebuilt library (feature `speech`) carries its
//! own, older copy of the same templates. The linker keeps one of each
//! function, the two halves disagree on the regex's layout, and the process
//! aborts (`std::bad_alloc`) before `main` -- the desktop app, which has
//! both, would not start. Without `common`, real speech and real decisions
//! were both run in one binary and gave what each gives alone.
//!
//! # Safety
//!
//! A [`Model`] is read-only once loaded and llama.cpp shares one between
//! threads. A [`Context`] stays with the thread that made it (a raw pointer
//! makes it neither `Send` nor `Sync`), and `local::decide` makes one per
//! question under its lock, since making two from one model at once is not
//! safe. Every pointer below comes from llama.cpp, is checked for null where
//! llama.cpp can return one, and is freed exactly once by the value that
//! owns it.

use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::OnceLock;

use llama_cpp_sys_2 as sys;

/// llama.cpp's process-wide state, started once, its log sent to `tracing`
/// under the `llama.cpp` target.
fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| unsafe {
        sys::llama_log_set(Some(log), std::ptr::null_mut());
        sys::llama_backend_init();
    });
}

/// ggml's log levels, as `ggml_log_level` numbers them.
const LOG_WARN: sys::ggml_log_level = 3;
const LOG_ERROR: sys::ggml_log_level = 4;

unsafe extern "C" fn log(level: sys::ggml_log_level, text: *const c_char, _: *mut c_void) {
    if text.is_null() {
        return;
    }
    // SAFETY: llama.cpp hands a NUL-terminated string valid for this call.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }
    match level {
        LOG_ERROR => tracing::error!(target: "llama.cpp", "{text}"),
        LOG_WARN => tracing::warn!(target: "llama.cpp", "{text}"),
        _ => tracing::debug!(target: "llama.cpp", "{text}"),
    }
}

/// A loaded GGUF.
pub struct Model(NonNull<sys::llama_model>);

// SAFETY: see this module's doc -- a model is not changed after it loads.
unsafe impl Send for Model {}
unsafe impl Sync for Model {}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: loaded by `load`, freed once, here.
        unsafe { sys::llama_model_free(self.0.as_ptr()) }
    }
}

impl Model {
    /// Load `path`, memory-mapped, every layer on the GPU where this build
    /// has one (Metal on Apple Silicon) -- llama.cpp's own defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        init();
        let path = path.to_str().ok_or("the model's path is not text")?;
        let path = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: a valid C string and llama.cpp's own default parameters.
        let ptr = unsafe {
            sys::llama_model_load_from_file(path.as_ptr(), sys::llama_model_default_params())
        };
        NonNull::new(ptr).map(Self).ok_or_else(|| "llama.cpp could not load it".into())
    }

    fn vocab(&self) -> *const sys::llama_vocab {
        // SAFETY: a live model; the vocabulary lives as long as it does.
        unsafe { sys::llama_model_get_vocab(self.0.as_ptr()) }
    }

    /// The vocabulary's size: how many logits a token position has.
    pub fn n_vocab(&self) -> usize {
        // SAFETY: as `vocab`.
        unsafe { sys::llama_vocab_n_tokens(self.vocab()) }.max(0) as usize
    }

    /// One metadata value, as text. `None` when the key is absent.
    pub fn meta(&self, key: &str) -> Option<String> {
        let key = CString::new(key).ok()?;
        read_string(|buf, len| unsafe {
            // SAFETY: a live model, a valid key, and a buffer of `len` bytes.
            sys::llama_model_meta_val_str(self.0.as_ptr(), key.as_ptr(), buf, len)
        })
    }

    /// Every metadata key and value, as text.
    pub fn meta_entries(&self) -> Vec<(String, String)> {
        // SAFETY: a live model.
        let n = unsafe { sys::llama_model_meta_count(self.0.as_ptr()) };
        (0..n)
            .filter_map(|i| {
                let key = read_string(|buf, len| unsafe {
                    // SAFETY: as `meta`, by index.
                    sys::llama_model_meta_key_by_index(self.0.as_ptr(), i, buf, len)
                })?;
                let value = read_string(|buf, len| unsafe {
                    // SAFETY: as `meta`, by index.
                    sys::llama_model_meta_val_str_by_index(self.0.as_ptr(), i, buf, len)
                })?;
                Some((key, value))
            })
            .collect()
    }

    /// `text` as tokens, nothing added at either end. `special` reads
    /// spellings like `<|im_end|>` as the special tokens they name; without
    /// it they are plain text.
    pub fn tokenize(&self, text: &str, special: bool) -> Result<Vec<i32>, String> {
        let len = i32::try_from(text.len()).map_err(|_| "the text is too long".to_string())?;
        // A token is at least one byte, but a few tokenizers add some.
        let mut tokens = vec![0i32; text.len() + 8];
        for _ in 0..2 {
            // SAFETY: `text` is `len` bytes; `tokens` holds its own length.
            let n = unsafe {
                sys::llama_tokenize(
                    self.vocab(),
                    text.as_ptr().cast(),
                    len,
                    tokens.as_mut_ptr(),
                    tokens.len() as i32,
                    false,
                    special,
                )
            };
            if n >= 0 {
                tokens.truncate(n as usize);
                return Ok(tokens);
            }
            // Too small: `-n` is how many it needs.
            tokens.resize(n.unsigned_abs() as usize, 0);
        }
        Err("llama.cpp could not tokenize the text".into())
    }

    /// A context sized for one prompt of `size` tokens -- context, batch
    /// and micro-batch all `size`, since a prompt read both ways has to go
    /// through in one -- using `threads` threads.
    pub fn context(&self, size: u32, threads: i32) -> Result<Context<'_>, String> {
        // SAFETY: llama.cpp's own defaults, with the sizes and threads set.
        let ptr = unsafe {
            let mut params = sys::llama_context_default_params();
            params.n_ctx = size;
            params.n_batch = size;
            params.n_ubatch = size;
            params.n_seq_max = 1;
            params.n_threads = threads;
            params.n_threads_batch = threads;
            sys::llama_init_from_model(self.0.as_ptr(), params)
        };
        let ptr = NonNull::new(ptr).ok_or("llama.cpp could not make room for the prompt")?;
        Ok(Context { ptr, size: size as usize, model: PhantomData })
    }
}

/// Read a string llama.cpp writes snprintf-style: it answers the length it
/// needed, so a short buffer is tried once more at that length.
fn read_string(call: impl Fn(*mut c_char, usize) -> i32) -> Option<String> {
    let mut buf = vec![0u8; 256];
    for _ in 0..2 {
        let n = call(buf.as_mut_ptr().cast(), buf.len());
        let n = usize::try_from(n).ok()?;
        if n < buf.len() {
            buf.truncate(n);
            return String::from_utf8(buf).ok();
        }
        buf = vec![0u8; n + 1];
    }
    None
}

/// One prompt's worth of llama.cpp's working memory.
pub struct Context<'m> {
    ptr: NonNull<sys::llama_context>,
    size: usize,
    model: PhantomData<&'m Model>,
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        // SAFETY: made by `Model::context`, freed once, here.
        unsafe { sys::llama_free(self.ptr.as_ptr()) }
    }
}

impl Context<'_> {
    /// Run `tokens` through the model in one batch and answer the logits at
    /// the last of them, one per vocabulary entry. Refuses a prompt larger
    /// than the context, which llama.cpp would otherwise abort the process
    /// over.
    pub fn last_logits(&mut self, tokens: &[i32], n_vocab: usize) -> Result<&[f32], String> {
        if tokens.is_empty() || tokens.len() > self.size {
            return Err(format!("{} tokens do not fit a context of {}", tokens.len(), self.size));
        }
        let n = tokens.len() as i32;
        // SAFETY: a batch for `n` tokens of one sequence, every field
        // written for each of them before it is decoded, freed after.
        unsafe {
            let batch = sys::llama_batch_init(n, 0, 1);
            for (i, token) in tokens.iter().enumerate() {
                *batch.token.add(i) = *token;
                *batch.pos.add(i) = i as i32;
                *batch.n_seq_id.add(i) = 1;
                **batch.seq_id.add(i) = 0;
                *batch.logits.add(i) = i8::from(i + 1 == tokens.len());
            }
            let mut batch = batch;
            batch.n_tokens = n;
            let result = sys::llama_decode(self.ptr.as_ptr(), batch);
            sys::llama_batch_free(batch);
            if result != 0 {
                return Err(format!("llama.cpp could not read the prompt ({result})"));
            }
            let logits = sys::llama_get_logits_ith(self.ptr.as_ptr(), n - 1);
            if logits.is_null() {
                return Err("llama.cpp gave no answer for the prompt".into());
            }
            // SAFETY: one logit per vocabulary entry, valid until the next
            // decode or the context is freed -- both need `&mut self`.
            Ok(std::slice::from_raw_parts(logits, n_vocab))
        }
    }
}
