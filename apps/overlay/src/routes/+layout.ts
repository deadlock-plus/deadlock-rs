// Tauri loads a static bundle from disk and everything is driven by events from the Rust
// side, so there is nothing to render on a server and nothing to prerender per route.
export const prerender = true;
export const ssr = false;
