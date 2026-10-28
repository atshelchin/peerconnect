import { MARKDOWN } from '#lib/server/llms.ts';
import api from '../../../../../API.md?raw';

export const prerender = true;

/** The API reference as written, for agents and tools. */
export function GET() {
	return new Response(api, { headers: MARKDOWN });
}
