import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  // Tailwind has to run before SvelteKit so the generated utilities exist by the time the
  // Svelte plugin hands `app.css` on to be bundled.
  plugins: [tailwindcss(), sveltekit()],
  // Tauri points its dev window at a fixed port and cannot follow a fallback, so failing
  // loudly here beats a window that silently loads nothing.
  server: { port: 5173, strictPort: true },
  clearScreen: false
});
