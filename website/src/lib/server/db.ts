// The site's database (migrations/0001_accounts.sql). Every query is here,
// with bound parameters only.

import type { App, AppFields } from './apps.ts';
import { newAccountId, newAppId, newClientId } from './crypto.ts';
import type { GitHubUser } from './github.ts';

export interface Account {
	id: string;
	state: 'active' | 'suspended';
	client_limit: number;
	app_limit: number;
	login: string;
	avatar_url: string;
}

export interface ClientRow {
	id: string;
	account_id: string;
	app: string | null;
	installation: string | null;
	external_user: string | null;
	label: string;
	created_at: number;
}

export interface CodeRow {
	client_id: string;
	account_id: string;
	app: string;
	redirect_uri: string;
	code_challenge: string;
	rotate: number;
}

const now = () => Math.floor(Date.now() / 1000);
const CLIENT_COLUMNS = 'id, account_id, app, installation, external_user, label, created_at';

export async function audit(
	db: D1Database,
	account: string | null,
	action: string,
	detail: string,
	actor: string
) {
	await db
		.prepare(
			'INSERT INTO audit (at, account_id, action, detail, actor) VALUES (?1, ?2, ?3, ?4, ?5)'
		)
		.bind(now(), account, action, detail, actor)
		.run();
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

/** The account a GitHub user signs in to; created on their first sign-in. */
export async function signIn(db: D1Database, user: GitHubUser): Promise<string> {
	const at = now();
	const known = await db
		.prepare(
			"SELECT account_id FROM identities WHERE provider = 'github' AND provider_user_id = ?1"
		)
		.bind(user.id)
		.first<{ account_id: string }>();
	if (known) {
		await db.batch([
			db
				.prepare(
					"UPDATE identities SET login = ?2, avatar_url = ?3 WHERE provider = 'github' AND provider_user_id = ?1"
				)
				.bind(user.id, user.login, user.avatarUrl),
			db.prepare('UPDATE accounts SET last_seen_at = ?2 WHERE id = ?1').bind(known.account_id, at)
		]);
		await audit(db, known.account_id, 'sign-in', `github:${user.login}`, 'person');
		return known.account_id;
	}
	const account = newAccountId();
	await db.batch([
		db
			.prepare('INSERT INTO accounts (id, created_at, last_seen_at) VALUES (?1, ?2, ?2)')
			.bind(account, at),
		db
			.prepare(
				"INSERT INTO identities (provider, provider_user_id, account_id, login, avatar_url, created_at) VALUES ('github', ?1, ?2, ?3, ?4, ?5)"
			)
			.bind(user.id, account, user.login, user.avatarUrl, at)
	]);
	await audit(db, account, 'account-created', `github:${user.login}`, 'person');
	return account;
}

export async function account(db: D1Database, id: string): Promise<Account | null> {
	return db
		.prepare(
			"SELECT a.id, a.state, a.client_limit, a.app_limit, i.login, i.avatar_url FROM accounts a JOIN identities i ON i.account_id = a.id AND i.provider = 'github' WHERE a.id = ?1"
		)
		.bind(id)
		.first<Account>();
}

/** Whether `account` signs in as the owner's GitHub identity — the ID, never the login. */
export async function isAdmin(
	db: D1Database,
	account: string,
	adminGithubId: string
): Promise<boolean> {
	if (!/^\d+$/.test(adminGithubId)) return false;
	const row = await db
		.prepare(
			"SELECT 1 AS yes FROM identities WHERE account_id = ?1 AND provider = 'github' AND provider_user_id = ?2"
		)
		.bind(account, adminGithubId)
		.first<{ yes: number }>();
	return row !== null;
}

// ---------------------------------------------------------------------------
// Apps
// ---------------------------------------------------------------------------

interface AppRow extends Omit<App, 'redirect_uris'> {
	redirect_uris: string;
}

const APP_COLUMNS =
	'id, account_id, name, redirect_uris, loopback_path, secret_gen, user_limit, state, created_at';

function parseApp(row: AppRow | null): App | null {
	if (!row) return null;
	let uris: unknown;
	try {
		uris = JSON.parse(row.redirect_uris);
	} catch {
		uris = [];
	}
	return { ...row, redirect_uris: Array.isArray(uris) ? uris.map(String) : [] };
}

/** An app that has not been deleted, whoever's it is. */
export async function app(db: D1Database, id: string): Promise<App | null> {
	if (!/^app_[a-z2-7]{16}$/.test(id)) return null;
	return parseApp(
		await db
			.prepare(`SELECT ${APP_COLUMNS} FROM apps WHERE id = ?1 AND deleted_at IS NULL`)
			.bind(id)
			.first<AppRow>()
	);
}

/** `account`'s app `id`, or null. */
export async function ownApp(db: D1Database, account: string, id: string): Promise<App | null> {
	const found = await app(db, id);
	return found?.account_id === account ? found : null;
}

export async function appsOf(db: D1Database, account: string): Promise<App[]> {
	const { results } = await db
		.prepare(
			`SELECT ${APP_COLUMNS} FROM apps WHERE account_id = ?1 AND deleted_at IS NULL ORDER BY created_at`
		)
		.bind(account)
		.all<AppRow>();
	return results.map((r) => parseApp(r)!);
}

export async function originsOf(db: D1Database, app: string): Promise<string[]> {
	const { results } = await db
		.prepare('SELECT origin FROM app_origins WHERE app_id = ?1 ORDER BY origin')
		.bind(app)
		.all<{ origin: string }>();
	return results.map((r) => r.origin);
}

/** Whether a page at `origin` may exchange codes for `app` (or, with `app` null, for any active app). */
export async function originAllowed(
	db: D1Database,
	origin: string,
	app: string | null
): Promise<boolean> {
	const row = await db
		.prepare(
			"SELECT 1 AS yes FROM app_origins o JOIN apps a ON a.id = o.app_id WHERE o.origin = ?1 AND (?2 IS NULL OR o.app_id = ?2) AND a.state = 'active' AND a.deleted_at IS NULL LIMIT 1"
		)
		.bind(origin, app)
		.first<{ yes: number }>();
	return row !== null;
}

export class LimitReached extends Error {}

export async function createApp(db: D1Database, acct: Account, fields: AppFields): Promise<string> {
	const { n } = (await db
		.prepare('SELECT COUNT(*) AS n FROM apps WHERE account_id = ?1 AND deleted_at IS NULL')
		.bind(acct.id)
		.first<{ n: number }>())!;
	if (n >= acct.app_limit) throw new LimitReached();
	const id = newAppId();
	await db.batch([
		db
			.prepare(
				'INSERT INTO apps (id, account_id, name, redirect_uris, loopback_path, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)'
			)
			.bind(
				id,
				acct.id,
				fields.name,
				JSON.stringify(fields.redirectUris),
				fields.loopbackPath,
				now()
			),
		...fields.origins.map((o) =>
			db.prepare('INSERT INTO app_origins (app_id, origin) VALUES (?1, ?2)').bind(id, o)
		)
	]);
	await audit(db, acct.id, 'app-created', id, 'person');
	return id;
}

export async function updateApp(db: D1Database, account: string, id: string, fields: AppFields) {
	await db.batch([
		db
			.prepare(
				'UPDATE apps SET name = ?3, redirect_uris = ?4, loopback_path = ?5 WHERE id = ?1 AND account_id = ?2 AND deleted_at IS NULL'
			)
			.bind(id, account, fields.name, JSON.stringify(fields.redirectUris), fields.loopbackPath),
		db.prepare('DELETE FROM app_origins WHERE app_id = ?1').bind(id),
		...fields.origins.map((o) =>
			db.prepare('INSERT INTO app_origins (app_id, origin) VALUES (?1, ?2)').bind(id, o)
		)
	]);
	await audit(db, account, 'app-updated', id, 'person');
}

/** The app's secret moves to the next generation: the old one stops at once. */
export async function rotateAppSecret(db: D1Database, account: string, id: string) {
	await db
		.prepare(
			'UPDATE apps SET secret_gen = secret_gen + 1 WHERE id = ?1 AND account_id = ?2 AND deleted_at IS NULL'
		)
		.bind(id, account)
		.run();
	await audit(db, account, 'app-secret-rotated', id, 'person');
}

export async function deleteApp(db: D1Database, account: string, id: string) {
	await db.batch([
		db
			.prepare('UPDATE apps SET deleted_at = ?3 WHERE id = ?1 AND account_id = ?2')
			.bind(id, account, now()),
		db.prepare('DELETE FROM app_origins WHERE app_id = ?1').bind(id)
	]);
	await audit(db, account, 'app-deleted', id, 'person');
}

// ---------------------------------------------------------------------------
// Clients
// ---------------------------------------------------------------------------

/** The clients an account holds itself: its app connections and hand-made ones, not its apps' users. */
export async function clients(db: D1Database, account: string): Promise<ClientRow[]> {
	const { results } = await db
		.prepare(
			`SELECT ${CLIENT_COLUMNS} FROM clients WHERE account_id = ?1 AND external_user IS NULL AND deleted_at IS NULL ORDER BY created_at`
		)
		.bind(account)
		.all<ClientRow>();
	return results;
}

export async function client(
	db: D1Database,
	account: string,
	id: string
): Promise<ClientRow | null> {
	return db
		.prepare(
			`SELECT ${CLIENT_COLUMNS} FROM clients WHERE id = ?1 AND account_id = ?2 AND deleted_at IS NULL`
		)
		.bind(id, account)
		.first<ClientRow>();
}

/** Every client of an app that is not deleted, up to `limit`. */
export async function appClients(
	db: D1Database,
	app: string,
	limit = 500,
	offset = 0
): Promise<ClientRow[]> {
	const { results } = await db
		.prepare(
			`SELECT ${CLIENT_COLUMNS} FROM clients WHERE app = ?1 AND deleted_at IS NULL ORDER BY created_at, id LIMIT ?2 OFFSET ?3`
		)
		.bind(app, limit, offset)
		.all<ClientRow>();
	return results;
}

/** How many people connected an app themselves, and how many users its server issued tokens for. */
export async function appCounts(
	db: D1Database,
	app: string
): Promise<{ people: number; users: number; clients: number }> {
	const row = await db
		.prepare(
			'SELECT COUNT(DISTINCT CASE WHEN external_user IS NULL THEN account_id END) AS people, SUM(CASE WHEN external_user IS NOT NULL THEN 1 ELSE 0 END) AS users, COUNT(*) AS clients FROM clients WHERE app = ?1 AND deleted_at IS NULL'
		)
		.bind(app)
		.first<{ people: number; users: number | null; clients: number }>();
	return { people: row?.people ?? 0, users: row?.users ?? 0, clients: row?.clients ?? 0 };
}

export async function installationClient(
	db: D1Database,
	account: string,
	app: string,
	installation: string
): Promise<ClientRow | null> {
	return db
		.prepare(
			`SELECT ${CLIENT_COLUMNS} FROM clients WHERE account_id = ?1 AND app = ?2 AND installation = ?3 AND deleted_at IS NULL`
		)
		.bind(account, app, installation)
		.first<ClientRow>();
}

/**
 * The app installation connects as `client` (one of the account's) from now
 * on: the client it had lets go of it, and `client` becomes this app's for
 * this installation. Its token is then rotated by the exchange, so a device
 * that held it before stops.
 */
export async function bindClient(
	db: D1Database,
	account: string,
	client: string,
	app: string,
	installation: string
): Promise<void> {
	await db.batch([
		db
			.prepare(
				'UPDATE clients SET installation = NULL WHERE account_id = ?1 AND app = ?2 AND installation = ?3 AND id != ?4'
			)
			.bind(account, app, installation, client),
		db
			.prepare(
				'UPDATE clients SET app = ?2, installation = ?3 WHERE id = ?4 AND account_id = ?1 AND deleted_at IS NULL'
			)
			.bind(account, app, installation, client)
	]);
	await audit(db, account, 'client-bound', `${client} ${app}`, 'person');
}

/** The installation lets go of its client (it connects as a new one). */
export async function unbindInstallation(
	db: D1Database,
	account: string,
	app: string,
	installation: string
): Promise<void> {
	await db
		.prepare(
			'UPDATE clients SET installation = NULL WHERE account_id = ?1 AND app = ?2 AND installation = ?3'
		)
		.bind(account, app, installation)
		.run();
}

/** A new client of the account's own, within its `client_limit`. */
export async function createClient(
	db: D1Database,
	acct: Account,
	fields: { app: string | null; installation: string | null; label: string }
): Promise<string> {
	const { n } = (await db
		.prepare(
			'SELECT COUNT(*) AS n FROM clients WHERE account_id = ?1 AND external_user IS NULL AND deleted_at IS NULL'
		)
		.bind(acct.id)
		.first<{ n: number }>())!;
	if (n >= acct.client_limit) throw new LimitReached();
	const id = newClientId();
	await db
		.prepare(
			'INSERT INTO clients (id, account_id, app, installation, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)'
		)
		.bind(id, acct.id, fields.app, fields.installation, fields.label, now())
		.run();
	await audit(db, acct.id, 'client-created', `${id} ${fields.app ?? 'manual'}`, 'person');
	return id;
}

/** The client an app's server has for one of its users (`key`: userKey()), if any. */
export async function userClient(
	db: D1Database,
	app: string,
	key: string
): Promise<ClientRow | null> {
	return db
		.prepare(
			`SELECT ${CLIENT_COLUMNS} FROM clients WHERE app = ?1 AND external_user = ?2 AND deleted_at IS NULL`
		)
		.bind(app, key)
		.first<ClientRow>();
}

/** A client for one of an app's users, owned by the app's developer, within the app's `user_limit`. */
export async function createUserClient(db: D1Database, app: App, key: string): Promise<string> {
	const { n } = (await db
		.prepare(
			'SELECT COUNT(*) AS n FROM clients WHERE app = ?1 AND external_user IS NOT NULL AND deleted_at IS NULL'
		)
		.bind(app.id)
		.first<{ n: number }>())!;
	if (n >= app.user_limit) throw new LimitReached();
	const id = newClientId();
	await db
		.prepare(
			"INSERT INTO clients (id, account_id, app, external_user, label, created_at) VALUES (?1, ?2, ?3, ?4, 'server-issued', ?5)"
		)
		.bind(id, app.account_id, app.id, key, now())
		.run();
	return id;
}

export async function deleteClient(db: D1Database, id: string, actor: string, account: string) {
	await db
		.prepare(
			// It lets go of its app installation, which may connect again as a new client.
			'UPDATE clients SET deleted_at = ?2, installation = NULL WHERE id = ?1 AND deleted_at IS NULL'
		)
		.bind(id, now())
		.run();
	await audit(db, account, 'client-deleted', id, actor);
}

// ---------------------------------------------------------------------------
// Authorization codes
// ---------------------------------------------------------------------------

export const CODE_SECONDS = 120;

export async function saveCode(db: D1Database, codeHash: string, row: CodeRow) {
	await db
		.prepare(
			'INSERT INTO auth_codes (code_hash, client_id, account_id, app, redirect_uri, code_challenge, rotate, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)'
		)
		.bind(
			codeHash,
			row.client_id,
			row.account_id,
			row.app,
			row.redirect_uri,
			row.code_challenge,
			row.rotate,
			now() + CODE_SECONDS
		)
		.run();
}

/**
 * The code's row, used up in the same statement: a code works once, and
 * only before it expires. A second attempt finds nothing.
 */
export async function takeCode(db: D1Database, codeHash: string): Promise<CodeRow | null> {
	const at = now();
	return db
		.prepare(
			'UPDATE auth_codes SET used_at = ?2 WHERE code_hash = ?1 AND used_at IS NULL AND expires_at > ?2 RETURNING client_id, account_id, app, redirect_uri, code_challenge, rotate'
		)
		.bind(codeHash, at)
		.first<CodeRow>();
}

// ---------------------------------------------------------------------------
// The public trial
// ---------------------------------------------------------------------------

/**
 * Counts one trial token for `key` (an address's HMAC) and says whether it is
 * within `limit` per `windowSeconds`. One statement: concurrent requests
 * cannot both take the last one.
 */
export async function takeTrial(
	db: D1Database,
	key: string,
	limit: number,
	windowSeconds: number
): Promise<boolean> {
	const at = now();
	const row = await db
		.prepare(
			`INSERT INTO trial_limits (key, window_start, count) VALUES (?1, ?2, 1)
			 ON CONFLICT (key) DO UPDATE SET
			   count = CASE WHEN window_start <= ?2 - ?3 THEN 1 ELSE count + 1 END,
			   window_start = CASE WHEN window_start <= ?2 - ?3 THEN ?2 ELSE window_start END
			 RETURNING count`
		)
		.bind(key, at, windowSeconds)
		.first<{ count: number }>();
	// Now and then, forget addresses not seen for a day.
	if (Math.random() < 0.01) {
		await db
			.prepare('DELETE FROM trial_limits WHERE window_start < ?1')
			.bind(at - 86_400)
			.run();
	}
	return (row?.count ?? limit + 1) <= limit;
}

// ---------------------------------------------------------------------------
// The admin page (docs/self-serve.md, "The admin page")
// ---------------------------------------------------------------------------

export interface AdminAccount {
	id: string;
	login: string;
	created_at: number;
	last_seen_at: number;
	state: 'active' | 'suspended';
	client_limit: number;
	app_limit: number;
	apps: number;
	clients: number;
}

export async function adminAccounts(db: D1Database): Promise<AdminAccount[]> {
	const { results } = await db
		.prepare(
			"SELECT a.id, i.login, a.created_at, a.last_seen_at, a.state, a.client_limit, a.app_limit, (SELECT COUNT(*) FROM apps p WHERE p.account_id = a.id AND p.deleted_at IS NULL) AS apps, (SELECT COUNT(*) FROM clients c WHERE c.account_id = a.id AND c.deleted_at IS NULL) AS clients FROM accounts a JOIN identities i ON i.account_id = a.id AND i.provider = 'github' ORDER BY a.last_seen_at DESC LIMIT 1000"
		)
		.all<AdminAccount>();
	return results;
}

export interface AdminApp {
	id: string;
	name: string;
	login: string;
	state: 'active' | 'disabled';
	user_limit: number;
	created_at: number;
	clients: number;
}

export async function adminApps(db: D1Database): Promise<AdminApp[]> {
	const { results } = await db
		.prepare(
			"SELECT p.id, p.name, i.login, p.state, p.user_limit, p.created_at, (SELECT COUNT(*) FROM clients c WHERE c.app = p.id AND c.deleted_at IS NULL) AS clients FROM apps p JOIN identities i ON i.account_id = p.account_id AND i.provider = 'github' WHERE p.deleted_at IS NULL ORDER BY p.created_at DESC LIMIT 1000"
		)
		.all<AdminApp>();
	return results;
}

/** Every client whose use an account answers for: its own, and its apps' server-issued ones. */
export async function clientOwners(db: D1Database): Promise<Map<string, string>> {
	const { results } = await db
		.prepare('SELECT id, account_id FROM clients WHERE deleted_at IS NULL')
		.all<{ id: string; account_id: string }>();
	return new Map(results.map((r) => [r.id, r.account_id]));
}

/** The clients to revoke when an account is suspended, a batch at a time. */
export async function accountClients(
	db: D1Database,
	account: string,
	limit = 500,
	offset = 0
): Promise<ClientRow[]> {
	const { results } = await db
		.prepare(
			`SELECT ${CLIENT_COLUMNS} FROM clients WHERE account_id = ?1 AND deleted_at IS NULL ORDER BY created_at, id LIMIT ?2 OFFSET ?3`
		)
		.bind(account, limit, offset)
		.all<ClientRow>();
	return results;
}

export async function setAccountState(
	db: D1Database,
	account: string,
	state: 'active' | 'suspended',
	actor: string
) {
	await db.prepare('UPDATE accounts SET state = ?2 WHERE id = ?1').bind(account, state).run();
	await audit(
		db,
		account,
		state === 'suspended' ? 'account-suspended' : 'account-reactivated',
		'',
		actor
	);
}

export async function setAccountLimits(
	db: D1Database,
	account: string,
	clientLimit: number,
	appLimit: number,
	actor: string
) {
	await db
		.prepare('UPDATE accounts SET client_limit = ?2, app_limit = ?3 WHERE id = ?1')
		.bind(account, clientLimit, appLimit)
		.run();
	await audit(db, account, 'limits-set', `${clientLimit} ${appLimit}`, actor);
}

export async function setAppState(
	db: D1Database,
	app: string,
	state: 'active' | 'disabled',
	actor: string
) {
	await db.prepare('UPDATE apps SET state = ?2 WHERE id = ?1').bind(app, state).run();
	await audit(db, null, state === 'disabled' ? 'app-disabled' : 'app-enabled', app, actor);
}

export interface AuditRow {
	at: number;
	account_id: string | null;
	action: string;
	detail: string;
	actor: string;
}

export async function recentAudit(db: D1Database, limit = 100): Promise<AuditRow[]> {
	const { results } = await db
		.prepare('SELECT at, account_id, action, detail, actor FROM audit ORDER BY at DESC LIMIT ?1')
		.bind(limit)
		.all<AuditRow>();
	return results;
}
