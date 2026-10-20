# peerconnect.dev

The PeerConnect website: an introduction, a getting-started guide, the specification and a live demo, in English and Chinese. Built with SvelteKit, prerendered to static HTML, and served by Cloudflare Workers.

## Develop

The demo uses the JavaScript SDK from [`../js`](../js), so build that first (see the root README), then:

```sh
bun install          # copies ../js into node_modules: re-run it after rebuilding the SDK
bun run dev
```

| Command                                       | Does                                                            |
| --------------------------------------------- | --------------------------------------------------------------- |
| `bun run build`                               | Prerenders every page into `.svelte-kit/cloudflare`             |
| `bun run preview`                             | Serves the build with `wrangler dev` on <http://127.0.0.1:4317> |
| `bun run check`, `bun run lint`               | Type-checks; checks formatting and lints                        |
| `bunx vitest --run`                           | Unit tests: i18n, dictionaries, highlighting, spec rendering    |
| `bunx playwright test`                        | End-to-end: SEO tags, hreflang, sitemap, language switch, 404s  |
| `PEERCONNECT_DEMO_E2E=1 bunx playwright test` | Also pairs two browsers through the public relay                |
| `bun scripts/assets.mjs`                      | Regenerates the icons and Open Graph images in `static/`        |

## How it is put together

- **Languages.** English lives at `/`, Chinese at `/zh/…`. The `reroute` hook in `src/hooks.ts` maps both to the same routes, and pages read the language from the URL (`#lib/i18n/current.svelte.ts`). Strings are in `src/lib/i18n/en.ts` and `zh.ts`; the Chinese file is typed against the English one, and a unit test checks that both have the same strings and placeholders. The long-form guide is one component per language (`src/routes/docs/`). The specification is English only.
- **SEO.** Every page is static HTML with its own title, description, canonical URL, `hreflang` alternates, Open Graph and Twitter cards, and JSON-LD (`src/lib/components/Seo.svelte`). `sitemap.xml` lists every page with its alternates. Visitors who prefer the other language get a suggestion, never a redirect.
- **Specification.** `/spec` renders `../SPEC.md` at build time (`src/lib/server/spec.ts`), with anchors for every section and links for `§` references. It ships no JavaScript.
- **Demo.** `/demo` loads the SDK only on that page. Each browser tab is one peer; its room record is kept in `sessionStorage`.
- **Settings.** `src/lib/site.ts` holds the site URL, the public relay and the repository URL (links to the repository appear once it is set).

## Deploy

```sh
bun run build && bunx wrangler deploy
```

Then attach the `peerconnect.dev` custom domain to the `peerconnect-website` Worker in the Cloudflare dashboard (or add it under `routes` in `wrangler.jsonc`).
