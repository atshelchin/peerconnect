<script lang="ts">
	import ConsoleBar from '#lib/components/ConsoleBar.svelte';
	import { consoleText, fill } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const c = $derived(consoleText[i18n.lang]);
	const t = $derived(c.admin);
	const mb = (bytes: number) => `${(bytes / 1e6).toFixed(1)} MB`;
	const usd = (n: number) => `$${n.toFixed(n < 1 ? 4 : 2)}`;
	const date = (s: number) => new Date(s * 1000).toISOString().slice(0, 16).replace('T', ' ');
	const dateMs = (ms: number) => date(ms / 1000);
</script>

<svelte:head>
	<title>{c.title.admin} · PeerConnect</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<section class="container console">
	<ConsoleBar login={data.login} avatar={data.avatar} csrf={data.csrf} admin />
	<h1>{c.title.admin}</h1>
	{#if form?.done}<p class="note" role="status">{fill(t.done, { id: form.done })}</p>{/if}
	{#if form?.more}
		<form class="problems" method="POST" action={form.more.kind === 'app' ? '?/app' : '?/account'}>
			{t.more}
			<input type="hidden" name="csrf" value={data.csrf} />
			<input type="hidden" name={form.more.kind} value={form.more.id} />
			<input
				type="hidden"
				name="state"
				value={form.more.kind === 'app' ? 'disabled' : 'suspended'}
			/>
			<input type="hidden" name="offset" value={form.more.offset} />
			<button class="button danger" type="submit">{t.goOn}</button>
		</form>
	{/if}

	<h2>{t.alerts} ({data.alerts.length})</h2>
	<p class="muted">{t.alertsNote}</p>
	{#if data.alerts.length === 0}
		<p class="muted">{t.noAlerts}</p>
	{:else}
		<div class="table">
			<table>
				<tbody>
					{#each data.alerts as a (a.day + a.client + a.kind)}
						<tr data-alert={a.kind}
							><td>{dateMs(a.at)}</td><td>@{a.login}<br /><code>{a.client}</code></td><td
								>{a.kind}</td
							><td
								>{a.kind === 'bytes' ? mb(a.value) : a.value} ≥ {a.kind === 'bytes'
									? mb(a.threshold)
									: a.threshold}</td
							></tr
						>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}

	<h2>{t.days}</h2>
	<p class="muted">{t.costNote}</p>
	<div class="table">
		<table>
			<thead
				><tr
					><th>Day</th><th>{c.common.connections}</th><th>{c.common.traffic}</th><th>{t.cost}</th
					></tr
				></thead
			>
			<tbody>
				{#each data.days as d (d.day)}
					<tr
						><td>{d.day}</td><td>{d.connections}</td><td>{mb(d.bytes)}</td><td>{usd(d.cost)}</td
						></tr
					>
				{/each}
			</tbody>
		</table>
	</div>

	<h2>{t.top}</h2>
	<div class="table">
		<table>
			<thead
				><tr
					><th>{t.login}</th><th>{c.common.connections}</th><th>{c.common.minutes}</th><th
						>{c.common.traffic}</th
					><th>{t.cost}</th></tr
				></thead
			>
			<tbody>
				{#each data.top as a (a.id)}
					<tr
						><td>{a.login}</td><td>{a.connections}</td><td>{Math.round(a.minutes)}</td><td
							>{mb(a.bytes)}</td
						><td>{usd(a.cost)}</td></tr
					>
				{/each}
			</tbody>
		</table>
	</div>

	<h2>{t.accounts} ({data.accounts.length})</h2>
	<div class="table">
		<table>
			<thead
				><tr
					><th>{t.login}</th><th>{t.state}</th><th>{t.seen}</th><th>{t.apps}</th><th>{t.clients}</th
					><th>{t.use}</th><th>{t.limits}</th></tr
				></thead
			>
			<tbody>
				{#each data.accounts as a (a.id)}
					<tr data-account={a.id}>
						<td>@{a.login}<br /><code>{a.id}</code></td>
						<td>
							<form method="POST" action="?/account">
								<input type="hidden" name="csrf" value={data.csrf} />
								<input type="hidden" name="account" value={a.id} />
								{a.state}
								<button
									class="button ghost small"
									name="state"
									value={a.state === 'active' ? 'suspended' : 'active'}
									>{a.state === 'active' ? t.suspend : t.reactivate}</button
								>
							</form>
						</td>
						<td>{date(a.last_seen_at)}</td>
						<td>{a.apps}</td>
						<td>{a.clients}</td>
						<td
							>{a.usage
								? `${a.usage.connections} · ${mb(a.usage.bytes)} · ${usd(a.usage.cost)}`
								: '—'}</td
						>
						<td>
							<form class="limits" method="POST" action="?/account">
								<input type="hidden" name="csrf" value={data.csrf} />
								<input type="hidden" name="account" value={a.id} />
								<input
									name="client_limit"
									type="number"
									min="0"
									value={a.client_limit}
									aria-label="client limit"
								/>
								<input
									name="app_limit"
									type="number"
									min="0"
									value={a.app_limit}
									aria-label="app limit"
								/>
								<button class="button ghost small" type="submit">{t.setLimits}</button>
							</form>
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>

	<h2>{t.apps} ({data.apps.length})</h2>
	<div class="table">
		<table>
			<thead
				><tr><th>{t.apps}</th><th>{t.owner}</th><th>{t.clients}</th><th>{t.state}</th></tr></thead
			>
			<tbody>
				{#each data.apps as a (a.id)}
					<tr data-app={a.id}>
						<td>{a.name}<br /><code>{a.id}</code></td>
						<td>@{a.login}</td>
						<td>{a.clients}</td>
						<td>
							<form method="POST" action="?/app">
								<input type="hidden" name="csrf" value={data.csrf} />
								<input type="hidden" name="app" value={a.id} />
								{a.state}
								<button
									class="button ghost small"
									name="state"
									value={a.state === 'active' ? 'disabled' : 'active'}
									>{a.state === 'active' ? t.disable : t.enable}</button
								>
							</form>
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>

	<h2>{t.audit}</h2>
	<div class="table">
		<table>
			<tbody>
				{#each data.audit as row, i (i)}
					<tr
						><td>{date(row.at)}</td><td>{row.action}</td><td
							><code>{row.account_id ?? ''}</code> {row.detail}</td
						><td>{row.actor}</td></tr
					>
				{/each}
			</tbody>
		</table>
	</div>

	<h2>{t.issuance}</h2>
	<div class="table">
		<table>
			<tbody>
				{#each data.issuance as row, i (i)}
					<tr
						><td>{dateMs(row.at)}</td><td>{row.action}</td><td
							><code>{row.client}</code> gen {row.gen ?? '—'}</td
						><td>{row.actor}</td></tr
					>
				{/each}
			</tbody>
		</table>
	</div>
</section>

<style>
	.console {
		padding-bottom: 80px;
	}
	h2 {
		margin-top: 40px;
		font-size: 1.2rem;
	}
	.muted {
		color: var(--muted);
		font-size: 0.9rem;
	}
	.table {
		overflow-x: auto;
		border: 1px solid var(--line);
		border-radius: var(--radius-sm);
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}
	th,
	td {
		text-align: left;
		padding: 8px 10px;
		border-bottom: 1px solid var(--line);
		vertical-align: top;
	}
	code {
		font-family: var(--font-mono);
		font-size: 0.75rem;
	}
	.small {
		min-height: 28px;
		padding: 0 10px;
		font-size: 0.8rem;
	}
	.limits {
		display: flex;
		gap: 6px;
	}
	.limits input {
		width: 64px;
		font: inherit;
		padding: 2px 6px;
	}
	.note {
		border: 1px solid var(--accent);
		background: var(--accent-soft);
		border-radius: var(--radius-sm);
		padding: 10px 14px;
	}
	.problems {
		border: 1px solid var(--danger);
		color: var(--danger);
		border-radius: var(--radius-sm);
		padding: 10px 14px;
		display: flex;
		gap: 12px;
		align-items: center;
	}
</style>
