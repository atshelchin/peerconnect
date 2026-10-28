import { MARKDOWN } from '#lib/server/llms.ts';
import spec from '../../../../SPEC.md?raw';

export const prerender = true;

/** The specification as written, for agents and tools. */
export function GET() {
	return new Response(spec, { headers: MARKDOWN });
}
