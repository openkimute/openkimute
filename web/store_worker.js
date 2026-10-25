// The page's worker (index.html): a converted folder, dropped or chosen, copied into this site's OPFS as the pack
// kumite_converter's browser page writes (assets.pack, the files' bytes, then assets.idx, {"version": 1, "files":
// {"path": [offset, length]}, "done": []}), through sync access handles. Message: {files: [[path, File]], staged};
// replies {progress: bytes}, then {done: bytes} or {error, quota}. The index goes first and comes back last: no index,
// no pack. Staged (room for two copies): the new one is written beside the one kept (.new), which goes only once the
// new one is whole; else over it.
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

const STAGED = ['assets.pack.new', 'assets.idx.new'];

onmessage = async ({ data }) => {
  const root = await navigator.storage.getDirectory();
  const staged = !!data.staged;
  const [packName, idxName] = staged ? STAGED : ['assets.pack', 'assets.idx'];
  let pack = null, idx = null, result;
  try {
    // (a copy staged before and cut off: its room is this one's)
    for (const name of STAGED) await root.removeEntry(name).catch(() => {});
    if (!staged) await root.removeEntry('assets.idx').catch(() => {});
    pack = await handle(root, packName);
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
    idx = await handle(root, idxName);
    const text = new TextEncoder().encode(JSON.stringify({ version: 1, files, done: [] }));
    idx.truncate(0);
    if (idx.write(text, { at: 0 }) !== text.length) throw new DOMException('a write was cut short', 'QuotaExceededError');
    idx.flush();
    if (staged) {
      pack.close();
      idx.close();
      pack = idx = null;
      const [p, i] = [await root.getFileHandle(STAGED[0]), await root.getFileHandle(STAGED[1])];
      if (typeof p.move !== 'function') throw new DOMException('this browser cannot rename its files', 'NotSupportedError');
      // the one kept goes, the new one takes its names: the index last (no index, no pack)
      await root.removeEntry('assets.idx').catch(() => {});
      await root.removeEntry('assets.pack').catch(() => {});
      await p.move('assets.pack');
      await i.move('assets.idx');
    }
    result = { done: at };
  } catch (e) {
    // nothing half-copied stays in the browser's storage
    try { pack?.truncate(0); idx?.truncate(0); } catch { /* closed */ }
    result = { error: `${e?.name}: ${e?.message}`, quota: e?.name === 'QuotaExceededError' };
  } finally {
    pack?.close();
    idx?.close();
    // (staged and stopped: the copy kept before stays as it was)
    if (staged) for (const name of STAGED) await root.removeEntry(name).catch(() => {});
    // (an empty pack and index are no conversion: removed)
    if ((await root.getFileHandle('assets.idx').then((h) => h.getFile()).then((f) => f.size).catch(() => 0)) === 0) {
      for (const name of ['assets.idx', 'assets.pack']) await root.removeEntry(name).catch(() => {});
    }
  }
  // (only now: the page ends this worker on its answer)
  postMessage(result);
};
