// The page's worker (index.html): a converted folder, dropped or chosen, copied into this site's OPFS as the pack
// kumite_converter's browser page writes (assets.pack, the files' bytes, then assets.idx, {"version": 1, "files":
// {"path": [offset, length]}, "done": []}), through sync access handles. Message: {files: [[path, File]]}; replies
// {progress: bytes}, then {done: bytes} or {error, quota}. The index goes first and comes back last: no index, no pack.
const CHUNK = 16 << 20;
const reader = new FileReaderSync();

// (a worker of an earlier page, closed, lets go of its handle a moment later)
const handle = async (dir, name) => {
  const file = await dir.getFileHandle(name, { create: true });
  for (let tries = 0; ; tries++) {
    try { return await file.createSyncAccessHandle(); } catch (e) {
      if (e?.name !== 'NoModificationAllowedError' || tries >= 50) throw e;
      await new Promise((ok) => setTimeout(ok, 100));
    }
  }
};

onmessage = async ({ data }) => {
  const root = await navigator.storage.getDirectory();
  let pack = null, idx = null;
  try {
    await root.removeEntry('assets.idx').catch(() => {});
    pack = await handle(root, 'assets.pack');
    pack.truncate(0);
    const files = {};
    let at = 0, told = 0;
    for (const [path, file] of data.files) {
      for (let from = 0; from < file.size; from += CHUNK) {
        const bytes = new Uint8Array(reader.readAsArrayBuffer(file.slice(from, from + CHUNK)));
        if (pack.write(bytes, { at: at + from }) !== bytes.length) throw new DOMException('a write was cut short', 'QuotaExceededError');
      }
      files[path] = [at, file.size];
      at += file.size;
      if (at - told > 32 << 20) { postMessage({ progress: at }); told = at; }
    }
    pack.flush();
    idx = await handle(root, 'assets.idx');
    const text = new TextEncoder().encode(JSON.stringify({ version: 1, files, done: [] }));
    idx.truncate(0);
    if (idx.write(text, { at: 0 }) !== text.length) throw new DOMException('a write was cut short', 'QuotaExceededError');
    idx.flush();
    postMessage({ done: at });
  } catch (e) {
    // nothing half-copied stays in the browser's storage
    try { pack?.truncate(0); idx?.truncate(0); } catch { /* closed */ }
    postMessage({ error: `${e?.name}: ${e?.message}`, quota: e?.name === 'QuotaExceededError' });
  } finally {
    pack?.close();
    idx?.close();
    // (an empty pack and index are no conversion: removed)
    if ((await root.getFileHandle('assets.idx').then((h) => h.getFile()).then((f) => f.size).catch(() => 0)) === 0) {
      for (const name of ['assets.idx', 'assets.pack']) await root.removeEntry(name).catch(() => {});
    }
  }
};
