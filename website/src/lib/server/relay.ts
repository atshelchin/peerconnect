// The relay's internal API, over the Service Binding (docs/self-serve.md,
// "Issuance: only the relay signs"). The site never holds the token secret:
// it asks, the relay mints, and the relay keeps the issuance audit.

export interface RelayConfig {
	/** The Service Binding to the relay Worker. */
	binding: Fetcher;
	/** PEERCONNECT_SERVICE_SECRET, as the relay has it. */
	serviceSecret: string;
}

export interface Minted {
	token: string;
	gen: number;
	/** Epoch seconds. */
	exp: number;
}

export interface UsageDay {
	day: string;
	connections: number;
	connected_ms: number;
	frames_in: number;
	frames_out: number;
	bytes_in: number;
	bytes_out: number;
	rejections: number;
	limited: number;
}

/** A finished day's usage for one client, as the relay wrote it. */
export interface UsageRow extends UsageDay {
	client: string;
}

export interface IssuanceRow {
	at: number;
	client: string;
	action: string;
	gen: number | null;
	actor: string;
}

export class RelayError extends Error {
	constructor(
		readonly status: number,
		message: string
	) {
		super(message);
	}
}

/** About a year: tokens are long-lived and revocable. */
export const TOKEN_DAYS = 365;
/** The public trial's tokens (docs/self-serve.md, "The public trial"). */
export const TRIAL_HOURS = 2;
export const TRIAL_APP = 'trial';

const expiry = (seconds = TOKEN_DAYS * 24 * 3600) => Math.floor(Date.now() / 1000) + seconds;

async function internal<T>(config: RelayConfig, path: string, body?: unknown): Promise<T> {
	const response = await config.binding.fetch(`https://relay/internal/${path}`, {
		method: body === undefined ? 'GET' : 'POST',
		headers: { 'content-type': 'application/json', 'x-peerconnect-service': config.serviceSecret },
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	if (!response.ok) throw new RelayError(response.status, (await response.text()).slice(0, 200));
	return (await response.json()) as T;
}

/** The client's token at its current generation: the same one every time. */
export async function mint(
	config: RelayConfig,
	client: string,
	app: string | null,
	actor: string,
	seconds?: number
): Promise<Minted> {
	return internal(config, `clients/${client}/mint`, { exp: expiry(seconds), app, actor });
}

/** A token at the next generation: the old one stops, and its connections close. */
export async function rotate(config: RelayConfig, client: string, actor: string): Promise<Minted> {
	return internal(config, `clients/${client}/rotate`, { exp: expiry(), actor });
}

/** Every token of the client stops; a new one must be minted. */
export async function revoke(config: RelayConfig, client: string, actor: string) {
	return internal(config, `clients/${client}/revoke`, { actor });
}

/** The client is gone for good. */
export async function remove(config: RelayConfig, client: string, actor: string) {
	return internal(config, `clients/${client}/delete`, { actor });
}

/** The days the relay has not written out yet, today's included. */
export async function usage(config: RelayConfig, client: string): Promise<UsageDay[]> {
	return internal(config, `clients/${client}/usage`);
}

/** Finished days' usage, every client's (admin, and app totals). */
export async function usageDays(config: RelayConfig, days = 30): Promise<UsageRow[]> {
	return internal(config, `usage?days=${days}`);
}

export interface AlertRow {
	at: number;
	day: string;
	client: string;
	kind: 'connections' | 'bytes' | 'rejections' | 'limited';
	value: number;
	threshold: number;
}

/** The latest abuse alerts: a client's day crossing a threshold (admin). */
export async function alerts(config: RelayConfig, limit = 100): Promise<AlertRow[]> {
	return internal(config, `alerts?limit=${limit}`);
}

/** The relay's latest issuance audit rows (admin). */
export async function issuanceAudit(config: RelayConfig, limit = 100): Promise<IssuanceRow[]> {
	return internal(config, `audit?limit=${limit}`);
}
