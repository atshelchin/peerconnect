<script lang="ts">
	// Who is signed in, and the signed-in pages, at the top of each of them.
	import { consoleText } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';

	let {
		login,
		avatar = '',
		csrf,
		admin = false
	}: { login: string; avatar?: string; csrf: string; admin?: boolean } = $props();
	const c = $derived(consoleText[i18n.lang]);
	const links = $derived([
		{ path: '/account', label: c.common.account },
		{ path: '/developers', label: c.common.developers },
		...(admin ? [{ path: '/admin', label: c.common.admin }] : [])
	]);
</script>

<div class="bar">
	<div class="who">
		{#if avatar}<img src={avatar} alt="" width="32" height="32" />{/if}
		<span>{c.common.signedInAs} <strong>@{login}</strong></span>
	</div>
	<nav aria-label={c.common.account}>
		{#each links as link (link.path)}
			<a
				href={i18n.href(link.path)}
				aria-current={i18n.path.startsWith(link.path) ? 'page' : undefined}>{link.label}</a
			>
		{/each}
		<form method="POST" action="/auth/signout">
			<input type="hidden" name="csrf" value={csrf} />
			<button class="linklike" type="submit">{c.common.signOut}</button>
		</form>
	</nav>
</div>

<style>
	.bar {
		display: flex;
		flex-wrap: wrap;
		gap: 12px 24px;
		align-items: center;
		justify-content: space-between;
		padding-block: 20px;
		border-bottom: 1px solid var(--line);
		margin-bottom: 32px;
	}
	.who {
		display: flex;
		gap: 10px;
		align-items: center;
	}
	.who img {
		border-radius: 50%;
	}
	nav {
		display: flex;
		flex-wrap: wrap;
		gap: 18px;
		align-items: center;
	}
	nav a[aria-current='page'] {
		font-weight: 700;
		color: var(--ink);
	}
	.linklike {
		background: none;
		border: none;
		padding: 0;
		font: inherit;
		color: var(--muted);
		cursor: pointer;
		text-decoration: underline;
	}
</style>
