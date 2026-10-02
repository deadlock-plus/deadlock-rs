import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
export default {
  preprocess: vitePreprocess(),
  kit: {
    // Tauri serves a directory of files, so everything is prerendered and routed on the
    // client. `fallback` is what makes client-side routing work from a file:// origin.
    adapter: adapter({ fallback: 'index.html' })
  }
};
