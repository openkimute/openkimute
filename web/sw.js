// The page's service worker (index.html registers it): the installed game (web/manifest.webmanifest) starts offline
// too. The game data is never kept here: it is the player's copy in OPFS, or the server's assets/ (passed through).
// The game's two halves, fetched by their version (?v=, index.html), never change: from the cache first. Once both
// halves of a version are kept, it is the game offline (the cached pkg/version.txt names it) and an older version's
// go; until then the older stays whole. The rest of the page: from the network first, the cached copy offline.
const CACHE = 'openkumite';
const scope = new URL(self.registration.scope);
// what the start reads besides the game (each optional: a missing one is left out)
const SHELL = ['./', 'store_worker.js', 'preload.json', 'manifest.webmanifest', 'pkg/config.json',
  'fonts/big-shoulders-display.woff2', 'pkg/openkumite.svg', 'pkg/icon-192.png', 'pkg/icon-512.png', 'pkg/maskable-512.png', 'pkg/apple-touch-icon.png'];

const HALVES = ['pkg/openkumite.js', 'pkg/openkumite_bg.wasm'];
const VERSION = 'pkg/version.txt';

const keep = async (cache, url, r) => {
  if (r.ok && r.type === 'basic') await cache.put(url, r);
};

// a half of the game kept; both of its version here: they are the game offline, an older version's go
const keepHalf = async (cache, url, r) => {
  if (!r.ok || r.type !== 'basic') return;
  await cache.put(url, r);
  const v = new URL(url).searchParams.get('v');
  const both = await Promise.all(HALVES.map((p) => cache.match(new URL(`${p}?v=${v}`, scope).href)));
  if (!both.every(Boolean)) return;
  for (const k of await cache.keys()) {
    const old = new URL(k.url);
    if (old.searchParams.has('v') && old.searchParams.get('v') !== v) await cache.delete(k);
  }
  await cache.put(new URL(VERSION, scope).href, new Response(v, { headers: { 'content-type': 'text/plain' } }));
};

self.addEventListener('install', (e) => e.waitUntil((async () => {
  const cache = await caches.open(CACHE);
  await Promise.all(SHELL.map(async (path) => {
    const url = new URL(path, scope).href;
    await keep(cache, url, await fetch(url, { cache: 'no-store' }).catch(() => Response.error()));
  }));
  // the game itself, so the first visit's copy plays offline
  const r = await fetch(new URL(VERSION, scope).href, { cache: 'no-store' }).catch(() => null);
  const version = r?.ok ? (await r.text()).trim() : '';
  if (version) {
    await Promise.all(HALVES.map(async (path) => {
      const url = new URL(`${path}?v=${version}`, scope).href;
      if (!(await cache.match(url))) await keepHalf(cache, url, await fetch(url).catch(() => Response.error()));
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
      e.waitUntil(keepHalf(cache, req.url, r.clone()));
      return r;
    }
    try {
      const r = await fetch(req);
      // (the version is kept with its halves, not as it comes: offline it never names a game not here)
      if (!url.pathname.endsWith(`/${VERSION}`)) e.waitUntil(keep(cache, req.url, r.clone()));
      return r;
    } catch (err) {
      return (await cache.match(req, { ignoreSearch: true })) ?? Response.error();
    }
  })());
});
