// The page's service worker (index.html registers it): the installed game (web/manifest.webmanifest) starts offline
// too. The game data is never kept here: it is the player's copy in OPFS, or the server's assets/ (passed through).
// The game's two halves, fetched by their version (?v=, index.html), never change: from the cache first, and an older
// version's copy goes when the new one comes. The rest of the page: from the network first, the cached copy offline.
const CACHE = 'openkumite';
const scope = new URL(self.registration.scope);
// what the start reads besides the game (each optional: a missing one is left out)
const SHELL = ['./', 'store_worker.js', 'preload.json', 'manifest.webmanifest', 'pkg/config.json', 'pkg/version.txt',
  'fonts/big-shoulders-display.woff2', 'pkg/openkumite.svg', 'pkg/icon-192.png', 'pkg/icon-512.png', 'pkg/maskable-512.png', 'pkg/apple-touch-icon.png'];

const keep = async (cache, url, r) => {
  if (!r.ok || r.type !== 'basic') return;
  const u = new URL(url);
  if (u.searchParams.has('v')) {
    for (const k of await cache.keys()) {
      const old = new URL(k.url);
      if (old.pathname === u.pathname && old.search !== u.search) await cache.delete(k);
    }
  }
  await cache.put(url, r);
};

self.addEventListener('install', (e) => e.waitUntil((async () => {
  const cache = await caches.open(CACHE);
  await Promise.all(SHELL.map(async (path) => {
    const url = new URL(path, scope).href;
    await keep(cache, url, await fetch(url, { cache: 'no-store' }).catch(() => Response.error()));
  }));
  // the game itself, so the first visit's copy plays offline
  const version = (await (await cache.match(new URL('pkg/version.txt', scope).href))?.text())?.trim();
  if (version) {
    await Promise.all(['pkg/openkumite.js', 'pkg/openkumite_bg.wasm'].map(async (path) => {
      const url = new URL(`${path}?v=${version}`, scope).href;
      if (!(await cache.match(url))) await keep(cache, url, await fetch(url).catch(() => Response.error()));
    }));
  }
  await self.skipWaiting();
})()));
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

self.addEventListener('fetch', (e) => {
  const req = e.request;
  const url = new URL(req.url);
  if (req.method !== 'GET' || url.origin !== scope.origin || !url.pathname.startsWith(scope.pathname)) return;
  // the game data (a range: a movie or a sound read in parts) goes as it would without this worker
  if (url.pathname.startsWith(`${scope.pathname}assets/`) || req.headers.has('range')) return;
  e.respondWith((async () => {
    const cache = await caches.open(CACHE);
    if (url.searchParams.has('v')) {
      const hit = await cache.match(req);
      if (hit) return hit;
      const r = await fetch(req);
      e.waitUntil(keep(cache, req.url, r.clone()));
      return r;
    }
    try {
      const r = await fetch(req);
      e.waitUntil(keep(cache, req.url, r.clone()));
      return r;
    } catch (err) {
      return (await cache.match(req, { ignoreSearch: true })) ?? Response.error();
    }
  })());
});
