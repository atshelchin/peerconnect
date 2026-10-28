// What a client's use costs on Cloudflare, estimated from its exact daily
// counts (docs/self-serve.md, "Metering and alerts"). List prices as of
// 2026-10, Workers Paid; included allowances are left out, so the figures
// are what use beyond them costs. A rough estimate for setting plans, not a
// bill: change the prices here when Cloudflare does.

import type { UsageDay } from './relay.ts';

export const PRICES = {
	/** Worker requests, per million. */
	workerRequests: 0.3,
	/** Durable Object requests, per million. WebSocket messages in count 20 to one. */
	objectRequests: 0.15,
	/** Durable Object duration, per million GB-seconds (128 MB objects). */
	objectGbSeconds: 12.5
};

/**
 * Each connection: one Worker request, the room object, the client object's
 * admission, and its reports (one per 5 s of activity, at most; counted here
 * as one per minute connected). Incoming messages wake the room, 20 to a
 * request. Hibernated time is free; active time is taken as 1 ms per frame.
 */
export function estimate(
	day: Pick<UsageDay, 'connections' | 'connected_ms' | 'frames_in'>
): number {
	const workerRequests = day.connections;
	const objectRequests = day.connections * 2 + day.frames_in / 20 + day.connected_ms / 60_000;
	const gbSeconds = (day.frames_in / 1000) * 0.125;
	return (
		(workerRequests / 1e6) * PRICES.workerRequests +
		(objectRequests / 1e6) * PRICES.objectRequests +
		(gbSeconds / 1e6) * PRICES.objectGbSeconds
	);
}
