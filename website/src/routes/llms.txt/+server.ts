import { LLMS, TEXT } from '#lib/server/llms.ts';

export const prerender = true;

export function GET() {
	return new Response(LLMS, { headers: TEXT });
}
