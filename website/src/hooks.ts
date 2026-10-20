import type { Reroute } from '@sveltejs/kit/hooks';
import { unlocalize } from '#lib/i18n/index.ts';

// `/zh/...` renders the same routes as `/...`; pages read the language from the URL.
export const reroute: Reroute = ({ url }) => unlocalize(url.pathname);
