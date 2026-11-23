// Makes the app installable ("Add to home screen"). It caches nothing: every request goes to the server, so what you
// see is always what is running there. A friendly page shows when the server can't be reached.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', (e) => {
  if (e.request.mode !== 'navigate') return;
  e.respondWith(
    fetch(e.request).catch(
      () =>
        new Response(
          '<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><body style="font:16px system-ui;background:#0a1113;color:#cfe;padding:32px"><h2>Can’t reach Model Roamer</h2><p>Is the computer it runs on awake and on the same network (or Tailscale)? Pull down to try again.</p>',
          { headers: { 'content-type': 'text/html' } }
        )
    )
  );
});
