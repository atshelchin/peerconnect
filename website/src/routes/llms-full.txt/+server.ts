import { TEXT, llmsFull } from '#lib/server/llms.ts';
import api from '../../../../API.md?raw';
import spec from '../../../../SPEC.md?raw';

export const prerender = true;

export function GET() {
	return new Response(llmsFull(api, spec), { headers: TEXT });
}
