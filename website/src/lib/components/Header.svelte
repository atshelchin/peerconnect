<script lang="ts">
	import { afterNavigate } from '$app/navigation';
	import { ENGLISH_ONLY, LOCALE, localize } from '#lib/i18n/index.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { rememberLang } from '#lib/i18n/preference.ts';
	import { REPO_URL } from '#lib/site.ts';
	import Logo from './Logo.svelte';

	let menu: HTMLDetailsElement | undefined = $state();

	const t = $derived(i18n.t);
	const other = $derived(i18n.lang === 'en' ? 'zh' : 'en');
	// A page that exists in one language only switches to the other's home page.
	const switchTo = $derived(localize(ENGLISH_ONLY.includes(i18n.path) ? '/' : i18n.path, other));
	const links = $derived([
		{ path: '/docs', href: i18n.href('/docs'), label: t.nav.docs },
		{ path: '/docs/api', href: '/docs/api', label: t.nav.api },
		{ path: '/demo', href: i18n.href('/demo'), label: t.nav.demo },
		{ path: '/spec', href: '/spec', label: t.nav.spec },
		{ path: '/developers', href: i18n.href('/developers'), label: t.nav.developers }
	]);

	afterNavigate(() => {
		if (menu) menu.open = false;
	});
</script>

{#snippet items()}
	{#each links as link (link.path)}
		<li>
			<a href={link.href} aria-current={i18n.path === link.path ? 'page' : undefined}>
				{link.label}
			</a>
		</li>
	{/each}
	{#if REPO_URL}
		<li><a href={REPO_URL} rel="external">{t.nav.github}</a></li>
	{/if}
	<li>
		<a
			class="lang"
			href={switchTo}
			hreflang={LOCALE[other].tag}
			lang={LOCALE[other].tag}
			onclick={() => rememberLang(other)}
		>
			{other === 'zh' ? '中文' : 'EN'}
		</a>
	</li>
{/snippet}

<a class="skip" href="#main">{t.nav.skip}</a>
<header class="header">
	<div class="container bar">
		<a class="brand" href={i18n.href('/')} aria-label="PeerConnect — {t.nav.home}">
			<Logo />
			<span>PeerConnect</span>
		</a>
		<nav class="desktop" aria-label={t.nav.menu}>
			<ul>{@render items()}</ul>
		</nav>
		<details class="mobile" bind:this={menu}>
			<summary aria-label={t.nav.menu}>
				<svg width="22" height="22" viewBox="0 0 24 24" aria-hidden="true">
					<path
						d="M4 7h16M4 12h16M4 17h16"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
					/>
				</svg>
			</summary>
			<nav aria-label={t.nav.menu}>
				<ul>{@render items()}</ul>
			</nav>
		</details>
	</div>
</header>

<style>
	.skip {
		position: absolute;
		left: 12px;
		top: -48px;
		z-index: 100;
		padding: 8px 14px;
		border-radius: 8px;
		background: var(--ink);
		color: var(--bg);
	}

	.skip:focus {
		top: 12px;
	}

	.header {
		position: sticky;
		top: 0;
		z-index: 50;
		background: color-mix(in srgb, var(--bg) 82%, transparent);
		backdrop-filter: saturate(1.6) blur(14px);
		-webkit-backdrop-filter: saturate(1.6) blur(14px);
		border-bottom: 1px solid color-mix(in srgb, var(--line) 70%, transparent);
	}

	.bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		height: var(--header-h);
	}

	.brand {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		color: var(--ink);
		font-weight: 700;
		font-size: 1.08rem;
		letter-spacing: -0.01em;
		text-decoration: none;
	}

	ul {
		display: flex;
		align-items: center;
		gap: 4px;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	ul a {
		display: inline-flex;
		align-items: center;
		min-height: 40px;
		padding: 0 12px;
		border-radius: 999px;
		color: var(--ink-2);
		font-size: 0.95rem;
		font-weight: 550;
		text-decoration: none;
	}

	ul a:hover {
		color: var(--ink);
		background: var(--bg-soft);
	}

	ul a[aria-current='page'] {
		color: var(--ink);
		background: var(--bg-soft);
	}

	.lang {
		margin-left: 6px;
		border: 1px solid var(--line-strong);
		font-family: var(--font-sans);
	}

	.mobile {
		display: none;
		position: relative;
	}

	summary {
		display: grid;
		place-items: center;
		width: 44px;
		height: 44px;
		border-radius: 12px;
		color: var(--ink);
		cursor: pointer;
		list-style: none;
	}

	summary::-webkit-details-marker {
		display: none;
	}

	.mobile[open] summary {
		background: var(--bg-soft);
	}

	.mobile nav {
		position: absolute;
		right: 0;
		top: calc(100% + 8px);
		min-width: 200px;
		padding: 8px;
		background: var(--surface);
		border: 1px solid var(--line);
		border-radius: 14px;
		box-shadow: var(--shadow);
	}

	.mobile ul {
		flex-direction: column;
		align-items: stretch;
	}

	.mobile ul a {
		width: 100%;
		border-radius: 10px;
	}

	.mobile .lang {
		margin: 6px 0 0;
		justify-content: center;
	}

	@media (max-width: 640px) {
		.desktop {
			display: none;
		}

		.mobile {
			display: block;
		}
	}
</style>
