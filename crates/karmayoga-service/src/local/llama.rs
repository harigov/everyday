//! The few parts of llama.cpp a decision model and a Qwen3-ASR recogniser
//! need, called straight through `llama-cpp-sys-2`: load a GGUF, read its
//! metadata, tokenize, read the logits at the last token of one prompt, and
//! -- with `speech` as well -- hear a clip through `mtmd` and write what the
//! model answers. Feature `decisions`.
//!
//! # Graphics cards
//!
//! A build with a GPU backend -- Metal on Apple Silicon, Vulkan on Linux --
//! lists the cards it can drive in [`gpus`], and [`Model::load`] puts a
//! model's layers on them or keeps it off them entirely. Which, and what to
//! do when a card runs out of room, is `local::decide`'s business.
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

/// A graphics card llama.cpp can put a model on.
#[derive(Debug, Clone)]
pub struct Gpu {
    /// What it calls itself: "NVIDIA GeForce RTX 3070".
    pub name: String,
    /// Bytes of its memory nothing is using yet, this process or another.
    pub free: u64,
}

/// The cards a model goes on when [`Model::load`] is asked to use them:
/// every separate card, or the built-in ones where there is none, as
/// llama.cpp picks them. Empty in a build without a GPU backend, and on a
/// computer without a card it can drive.
pub fn gpus() -> Vec<Gpu> {
    init();
    let (mut cards, mut built_in) = (Vec::new(), Vec::new());
    // SAFETY: llama.cpp's own device list, read after `init`. Its devices
    // live as long as the process, and the description is a NUL-terminated
    // string they own.
    unsafe {
        for i in 0..sys::ggml_backend_dev_count() {
            let dev = sys::ggml_backend_dev_get(i);
            if dev.is_null() {
                continue;
            }
            let list = match sys::ggml_backend_dev_type(dev) {
                sys::GGML_BACKEND_DEVICE_TYPE_GPU => &mut cards,
                sys::GGML_BACKEND_DEVICE_TYPE_IGPU => &mut built_in,
                _ => continue,
            };
            let (mut free, mut total) = (0usize, 0usize);
            sys::ggml_backend_dev_memory(dev, &mut free, &mut total);
            let name = sys::ggml_backend_dev_description(dev);
            let name = if name.is_null() {
                "a graphics card".to_string()
            } else {
                CStr::from_ptr(name).to_string_lossy().trim().to_string()
            };
            list.push(Gpu { name, free: free as u64 });
        }
    }
    if cards.is_empty() { built_in } else { cards }
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
    /// Load `path`, memory-mapped. With `on_gpu`, every layer goes on the
    /// [`gpus`] -- llama.cpp's own default. Without, the model has no card
    /// at all, not just no layers on one: llama.cpp would otherwise still
    /// send a long prompt's multiplications over to a card it can see, which
    /// needs the room on it that a model is kept off it for.
    pub fn load(path: &Path, on_gpu: bool) -> Result<Self, String> {
        init();
        let path = path.to_str().ok_or("the model's path is not text")?;
        let path = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: a valid C string and llama.cpp's own default parameters,
        // with the placement set.
        let ptr = unsafe {
            let mut params = sys::llama_model_default_params();
            if !on_gpu {
                params.n_gpu_layers = 0;
                // One device, and "main_gpu" none of them: no devices.
                params.split_mode = sys::LLAMA_SPLIT_MODE_NONE;
                params.main_gpu = -1;
            }
            sys::llama_model_load_from_file(path.as_ptr(), params)
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
        self.context_batched(size, size, threads)
    }

    /// A context of `size` tokens read `batch` at a time, for a prompt read
    /// one way only -- a model that writes an answer after it -- which can
    /// go through in pieces.
    pub fn context_batched(
        &self,
        size: u32,
        batch: u32,
        threads: i32,
    ) -> Result<Context<'_>, String> {
        let batch = batch.clamp(1, size.max(1));
        // SAFETY: llama.cpp's own defaults, with the sizes and threads set.
        let ptr = unsafe {
            let mut params = sys::llama_context_default_params();
            params.n_ctx = size;
            params.n_batch = batch;
            params.n_ubatch = batch;
            params.n_seq_max = 1;
            params.n_threads = threads;
            params.n_threads_batch = threads;
            sys::llama_init_from_model(self.0.as_ptr(), params)
        };
        let ptr = NonNull::new(ptr).ok_or("llama.cpp could not make room for the prompt")?;
        Ok(Context { ptr, size: size as usize, batch: batch as usize, model: PhantomData })
    }

    /// `token` as the text it stands for. Special tokens come out empty.
    fn piece(&self, token: i32) -> Vec<u8> {
        let mut buf = vec![0u8; 64];
        for _ in 0..2 {
            // SAFETY: a live vocabulary and a buffer of its own length.
            let n = unsafe {
                sys::llama_token_to_piece(
                    self.vocab(),
                    token,
                    buf.as_mut_ptr().cast(),
                    buf.len() as i32,
                    0,
                    false,
                )
            };
            if n >= 0 {
                buf.truncate(n as usize);
                return buf;
            }
            buf.resize(n.unsigned_abs() as usize, 0);
        }
        Vec::new()
    }

    /// Whether `token` ends what the model writes.
    fn ends(&self, token: i32) -> bool {
        // SAFETY: a live vocabulary.
        unsafe { sys::llama_vocab_is_eog(self.vocab(), token) }
    }
}

/// llama.cpp's encoder for what a model hears (its multimodal projector,
/// `mtmd`), loaded beside the [`Model`] it feeds. Feature `speech` as well
/// as `decisions`: only a recogniser asks for one.
#[cfg(feature = "speech")]
pub struct Hearing(NonNull<sys::mtmd_context>);

// SAFETY: as `Model` -- the encoder's weights are not changed after it
// loads. Encoding a clip is not safe from two threads at once; whoever
// holds one runs one clip at a time (`speech::qwen`).
#[cfg(feature = "speech")]
unsafe impl Send for Hearing {}
#[cfg(feature = "speech")]
unsafe impl Sync for Hearing {}

#[cfg(feature = "speech")]
impl Drop for Hearing {
    fn drop(&mut self) {
        // SAFETY: made by `load`, freed once, here.
        unsafe { sys::mtmd_free(self.0.as_ptr()) }
    }
}

#[cfg(feature = "speech")]
impl Hearing {
    /// Load the encoder at `path` for `model`, on the [`gpus`] or off them
    /// as `model` was. Refuses one that cannot hear. `model` must outlive
    /// it: whoever holds both drops this first.
    pub fn load(path: &Path, model: &Model, on_gpu: bool, threads: i32) -> Result<Self, String> {
        init();
        let path = path.to_str().ok_or("the encoder's path is not text")?;
        let path = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: a valid C string, a live model, and mtmd's own default
        // parameters with the placement and threads set.
        let ptr = unsafe {
            sys::mtmd_log_set(Some(log), std::ptr::null_mut());
            sys::mtmd_helper_log_set(Some(log), std::ptr::null_mut());
            let mut params = sys::mtmd_context_params_default();
            params.use_gpu = on_gpu;
            params.print_timings = false;
            params.n_threads = threads;
            params.warmup = false;
            sys::mtmd_init_from_file(path.as_ptr(), model.0.as_ptr(), params)
        };
        let hearing =
            NonNull::new(ptr).map(Self).ok_or("llama.cpp could not load the audio encoder")?;
        // SAFETY: a live context.
        if !unsafe { sys::mtmd_support_audio(hearing.0.as_ptr()) } {
            return Err("the encoder does not take audio".into());
        }
        Ok(hearing)
    }

    /// The rate, in samples a second, the audio it is given must be at.
    pub fn sample_rate(&self) -> u32 {
        // SAFETY: a live context.
        u32::try_from(unsafe { sys::mtmd_get_audio_sample_rate(self.0.as_ptr()) }).unwrap_or(0)
    }

    /// The text that stands for the clip in a prompt, where it goes.
    pub fn marker() -> &'static str {
        // SAFETY: a static, NUL-terminated string mtmd owns.
        unsafe { CStr::from_ptr(sys::mtmd_default_marker()) }.to_str().unwrap_or("<__media__>")
    }

    /// `prompt`, its [`Self::marker`] read as `audio` (mono samples in
    /// -1..1 at [`Self::sample_rate`]), as llama.cpp's pieces, and how many
    /// positions they take in a context.
    pub fn tokenize(&self, prompt: &str, audio: &[f32]) -> Result<Heard, String> {
        if audio.len() < 2 {
            // mtmd asserts -- aborting the process -- on an empty clip.
            return Err("there is no audio to hear".into());
        }
        let text = CString::new(prompt).map_err(|e| e.to_string())?;
        // SAFETY: the bitmap copies `audio`; the chunks are freed by `Heard`
        // and the bitmap here, once each. The prompt is a valid C string of
        // the length given, read with special tokens on: it is ours, and
        // spells `<|im_start|>` and the like on purpose.
        unsafe {
            let bitmap = sys::mtmd_bitmap_init_from_audio(audio.len(), audio.as_ptr());
            let bitmap = NonNull::new(bitmap).ok_or("llama.cpp could not take the audio")?;
            let chunks = NonNull::new(sys::mtmd_input_chunks_init());
            let Some(chunks) = chunks else {
                sys::mtmd_bitmap_free(bitmap.as_ptr());
                return Err("llama.cpp could not make room for the prompt".into());
            };
            let heard = Heard(chunks);
            let input = sys::mtmd_input_text {
                text: text.as_ptr(),
                text_len: prompt.len(),
                add_special: false,
                parse_special: true,
            };
            let bitmaps = [bitmap.as_ptr() as *const sys::mtmd_bitmap];
            let result =
                sys::mtmd_tokenize(self.0.as_ptr(), heard.0.as_ptr(), &input, bitmaps.as_ptr(), 1);
            sys::mtmd_bitmap_free(bitmap.as_ptr());
            match result {
                0 => Ok(heard),
                1 => Err("the prompt has no place for the audio".into()),
                n => Err(format!("llama.cpp could not read the audio ({n})")),
            }
        }
    }
}

/// A prompt with a clip in it, read into llama.cpp's pieces by
/// [`Hearing::tokenize`].
#[cfg(feature = "speech")]
pub struct Heard(NonNull<sys::mtmd_input_chunks>);

#[cfg(feature = "speech")]
impl Drop for Heard {
    fn drop(&mut self) {
        // SAFETY: made by `Hearing::tokenize`, freed once, here.
        unsafe { sys::mtmd_input_chunks_free(self.0.as_ptr()) }
    }
}

#[cfg(feature = "speech")]
impl Heard {
    /// How many positions it takes in a context.
    pub fn positions(&self) -> usize {
        // SAFETY: live chunks.
        unsafe { sys::mtmd_helper_get_n_pos(self.0.as_ptr()) }.max(0) as usize
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
    /// How many tokens go through in one piece.
    batch: usize,
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
    /// Read `heard` -- a prompt with a clip in it -- and write what the
    /// model answers after it, the likeliest token each time, until it ends
    /// or has written `max_tokens`. Refuses a prompt and answer that would
    /// not fit the context, which llama.cpp would otherwise abort over.
    ///
    /// `at_least` tokens are written whether the model would end sooner or
    /// not, the likeliest token that does not end it standing in -- for
    /// timing a model on audio it has nothing to say about.
    #[cfg(feature = "speech")]
    pub fn write_after(
        &mut self,
        model: &Model,
        hearing: &Hearing,
        heard: &Heard,
        max_tokens: usize,
        at_least: usize,
    ) -> Result<String, String> {
        let positions = heard.positions();
        if positions + max_tokens > self.size {
            return Err(format!(
                "{positions} positions and {max_tokens} more do not fit a context of {}",
                self.size
            ));
        }
        let mut past: sys::llama_pos = 0;
        // SAFETY: live contexts, chunks made by `hearing` for this model,
        // and a batch size above zero (`context_batched` clamps it).
        let read = unsafe {
            sys::mtmd_helper_eval_chunks(
                hearing.0.as_ptr(),
                self.ptr.as_ptr(),
                heard.0.as_ptr(),
                0,
                0,
                self.batch as i32,
                true,
                &mut past,
            )
        };
        if read != 0 {
            return Err(format!("llama.cpp could not hear the audio ({read})"));
        }
        let n_vocab = model.n_vocab();
        let mut written = Vec::new();
        for n in 0..max_tokens {
            // SAFETY: the logits of the last token decoded, one per
            // vocabulary entry, read before the next decode.
            let logits = unsafe {
                let logits = sys::llama_get_logits_ith(self.ptr.as_ptr(), -1);
                if logits.is_null() {
                    return Err("llama.cpp gave no answer".into());
                }
                std::slice::from_raw_parts(logits, n_vocab)
            };
            let mut token = argmax(logits);
            if model.ends(token) {
                if n >= at_least {
                    break;
                }
                let mut others = logits.to_vec();
                while model.ends(token) {
                    others[token as usize] = f32::NEG_INFINITY;
                    token = argmax(&others);
                }
            }
            written.extend(model.piece(token));
            // SAFETY: a batch of one token at the next position, its logits
            // asked for, freed after.
            unsafe {
                let mut batch = sys::llama_batch_init(1, 0, 1);
                *batch.token = token;
                *batch.pos = past;
                *batch.n_seq_id = 1;
                **batch.seq_id = 0;
                *batch.logits = 1;
                batch.n_tokens = 1;
                let result = sys::llama_decode(self.ptr.as_ptr(), batch);
                sys::llama_batch_free(batch);
                if result != 0 {
                    return Err(format!("llama.cpp could not go on ({result})"));
                }
            }
            past += 1;
        }
        Ok(String::from_utf8_lossy(&written).into_owned())
    }
}

/// The index of the largest of `values`; the first, when several tie.
#[cfg(feature = "speech")]
fn argmax(values: &[f32]) -> i32 {
    let mut best = 0;
    for (i, v) in values.iter().enumerate() {
        if *v > values[best] {
            best = i;
        }
    }
    best as i32
}
