//! Files the player chooses: saving one (a replay or a video exported) and opening some (replays imported).
//! Natively the system's own dialogs (rfd: on Linux the desktop's portal); in a browser a download, and the page's
//! file picker, whose files arrive a moment later ([`take_opened`]).

/// What the files are, for the dialogs: its name, and its files' extension.
pub struct Kind {
    /// (a browser's picker filters by the extension alone; Android has none)
    #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(dead_code))]
    pub name: &'static str,
    pub ext: &'static str,
}

pub const REPLAY: Kind = Kind { name: "OpenKumite replay", ext: "okreplay" };
#[cfg(not(target_arch = "wasm32"))]
pub const VIDEO: Kind = Kind { name: "MP4 video", ext: "mp4" };

/// Where the player wants a new file `name` of `kind` (None: cancelled). Native only: a browser downloads.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn save_path(name: &str, kind: &Kind) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().set_file_name(name).add_filter(kind.name, &[kind.ext]).save_file()
}

/// Android: no dialog; the file goes in the app's folder (beside the save).
// shortcut: no system file picker (that is Java's, the Storage Access Framework), upgrade with an import/export UI
#[cfg(target_os = "android")]
pub fn save_path(name: &str, _kind: &Kind) -> Option<std::path::PathBuf> {
    Some(crate::save::save_dir().join(name))
}

/// Save `bytes` as a file the player names (natively a dialog; in a browser, a download): where it went, or None when
/// cancelled.
pub fn save(name: &str, kind: &Kind, bytes: &[u8]) -> anyhow::Result<Option<String>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = save_path(name, kind) else { return Ok(None) };
        std::fs::write(&path, bytes)?;
        Ok(Some(path.display().to_string()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = kind;
        web::download(name, bytes)?;
        Ok(Some(format!("Downloads: {name}")))
    }
}

/// Ask for files of `kind` to open: natively the dialog, its files at once; in a browser the picker, its files
/// later from [`take_opened`].
pub fn open(kind: &Kind) -> Vec<(String, Vec<u8>)> {
    #[cfg(target_os = "android")]
    {
        log::warn!("no file picker on Android: {} files cannot be opened", kind.ext);
        Vec::new()
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        let picked = rfd::FileDialog::new().add_filter(kind.name, &[kind.ext]).pick_files().unwrap_or_default();
        picked.into_iter().filter_map(|p| Some((p.file_name()?.to_string_lossy().into_owned(), std::fs::read(&p).ok()?))).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::pick(kind.ext);
        Vec::new()
    }
}

/// Files the browser's picker has read since (natively none: the dialog gives them at once).
pub fn take_opened() -> Vec<(String, Vec<u8>)> {
    #[cfg(target_arch = "wasm32")]
    {
        web::OPENED.with(|o| std::mem::take(&mut *o.borrow_mut()))
    }
    #[cfg(not(target_arch = "wasm32"))]
    Vec::new()
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    thread_local! {
        pub static OPENED: RefCell<Vec<(String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    }

    fn err(e: JsValue) -> anyhow::Error {
        anyhow::anyhow!("{e:?}")
    }

    /// The bytes as a download named `name`.
    pub fn download(name: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let doc = web_sys::window().and_then(|w| w.document()).ok_or_else(|| anyhow::anyhow!("no page"))?;
        let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("application/octet-stream");
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(err)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;
        let a: web_sys::HtmlAnchorElement = doc.create_element("a").map_err(err)?.dyn_into().map_err(|_| anyhow::anyhow!("no link"))?;
        a.set_href(&url);
        a.set_download(name);
        a.click();
        // (the browser has the file by the time this runs)
        let revoke = Closure::once_into_js(move || {
            let _ = web_sys::Url::revoke_object_url(&url);
        });
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 60_000);
        }
        Ok(())
    }

    /// The page's file picker for `.ext` files; each one picked read into [`OPENED`].
    pub fn pick(ext: &str) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
        let Ok(input) = doc.create_element("input").map(|e| e.unchecked_into::<web_sys::HtmlInputElement>()) else { return };
        input.set_type("file");
        input.set_accept(&format!(".{ext}"));
        input.set_multiple(true);
        let picker = input.clone();
        let changed = Closure::<dyn FnMut()>::new(move || {
            let Some(files) = picker.files() else { return };
            for i in 0..files.length() {
                let Some(file) = files.get(i) else { continue };
                let name = file.name();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(buf) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                        let bytes = js_sys::Uint8Array::new(&buf).to_vec();
                        OPENED.with(|o| o.borrow_mut().push((name, bytes)));
                    }
                });
            }
        });
        input.set_onchange(Some(changed.as_ref().unchecked_ref()));
        changed.forget();
        input.click();
    }
}
