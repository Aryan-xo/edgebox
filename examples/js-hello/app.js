// An edgebox function in JavaScript: the standard `fetch` event, compiled to a
// wasi:http component with jco (ComponentizeJS + StarlingMonkey).
addEventListener("fetch", (event) => {
  const { pathname, search } = new URL(event.request.url);
  console.log(`handling ${event.request.method} ${pathname}`);
  event.respondWith(
    new Response(`Hello from JavaScript! You requested ${pathname}${search}\n`, {
      headers: { "content-type": "text/plain" },
    }),
  );
});
