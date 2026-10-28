<script lang="ts">
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { RELAY_URL, REPO_URL } from '#lib/site.ts';
	import Logo from './Logo.svelte';

	const t = $derived(i18n.t);
</script>

<footer class="footer">
	<div class="container grid">
		<div class="brand">
			<a class="name" href={i18n.href('/')}>
				<Logo size={24} />
				<span>PeerConnect</span>
			</a>
			<p>{t.footer.tagline}</p>
			<p class="small">{t.footer.license}</p>
		</div>
		<nav aria-label={t.footer.project}>
			<h2>{t.footer.project}</h2>
			<ul>
				<li><a href={i18n.href('/docs')}>{t.nav.docs}</a></li>
				<li><a href="/docs/api">{t.nav.api}</a></li>
				<li><a href="/llms.txt">llms.txt</a></li>
				<li><a href={i18n.href('/demo')}>{t.nav.demo}</a></li>
				<li><a href="/spec">{t.nav.spec}</a></li>
				{#if REPO_URL}
					<li><a href={REPO_URL} rel="external">{t.nav.github}</a></li>
				{/if}
			</ul>
		</nav>
		<div>
			<h2>{t.footer.resources}</h2>
			<ul>
				<li><a href={i18n.href('/docs') + '#relay'}>{t.home.relay.guide}</a></li>
				<li><a href={i18n.href('/docs') + '#limitations'}>{t.home.security.limitations}</a></li>
				<li>
					<span class="label">{t.footer.publicRelay}</span>
					<code>{RELAY_URL.replace('wss://', '')}</code>
				</li>
			</ul>
		</div>
	</div>
	<div class="container">
		<div class="base">
			<span>© {new Date().getFullYear()} PeerConnect</span>
			<span class="status">{t.footer.status}</span>
		</div>
	</div>
</footer>

<style>
	.footer {
		margin-top: auto;
		border-top: 1px solid var(--line);
		background: var(--bg-soft);
		font-size: 0.95rem;
	}

	.grid {
		display: grid;
		grid-template-columns: 2fr 1fr 1.4fr;
		gap: 40px;
		padding-block: 56px 40px;
	}

	.brand {
		display: grid;
		gap: 12px;
		align-content: start;
		max-width: 360px;
		color: var(--ink-2);
	}

	.name {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		color: var(--ink);
		font-weight: 700;
		text-decoration: none;
	}

	.small {
		color: var(--muted);
		font-size: 0.88rem;
	}

	h2 {
		margin-bottom: 14px;
		font-size: 0.78rem;
		font-family: var(--font-mono);
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--muted);
	}

	ul {
		display: grid;
		gap: 10px;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	ul a {
		color: var(--ink-2);
		text-decoration: none;
	}

	ul a:hover {
		color: var(--accent);
	}

	.label {
		display: block;
		color: var(--muted);
		font-size: 0.85rem;
	}

	.base {
		display: flex;
		justify-content: space-between;
		gap: 16px;
		padding-block: 20px 28px;
		border-top: 1px solid var(--line);
		color: var(--muted);
		font-size: 0.85rem;
	}

	.status {
		font-family: var(--font-mono);
	}

	@media (max-width: 760px) {
		.grid {
			grid-template-columns: 1fr 1fr;
		}

		.brand {
			grid-column: 1 / -1;
		}
	}

	@media (max-width: 480px) {
		.grid {
			grid-template-columns: 1fr;
			gap: 32px;
		}
	}
</style>
