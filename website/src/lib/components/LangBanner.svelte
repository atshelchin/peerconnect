<script lang="ts">
	import { onMount } from 'svelte';
	import { ENGLISH_ONLY, LOCALE, dict, localize, type Lang } from '#lib/i18n/index.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { preferredLang, rememberLang } from '#lib/i18n/preference.ts';

	// Suggests the other language to visitors who prefer it. Never redirects:
	// search engines and shared links always get the page they asked for.
	let suggested: Lang | null = $state(null);

	onMount(() => {
		const preferred = preferredLang();
		if (preferred !== i18n.lang && !ENGLISH_ONLY.includes(i18n.path)) suggested = preferred;
	});

	function dismiss() {
		if (suggested) rememberLang(i18n.lang);
		suggested = null;
	}
</script>

{#if suggested}
	{@const t = dict(suggested).banner}
	<div class="banner" role="region" aria-label={t.text} lang={LOCALE[suggested].tag}>
		<div class="inner">
			<span>{t.text}</span>
			<a
				class="button primary"
				href={localize(i18n.path, suggested)}
				hreflang={LOCALE[suggested].tag}
				onclick={() => suggested && rememberLang(suggested)}
			>
				{t.action}
			</a>
			<button class="close" type="button" onclick={dismiss} aria-label={t.dismiss}>
				<svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
					<path
						d="M6 6l12 12M18 6L6 18"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
					/>
				</svg>
			</button>
		</div>
	</div>
{/if}

<style>
	/* Floats over the page, so that appearing after load shifts nothing. */
	.banner {
		position: fixed;
		left: 50%;
		bottom: 16px;
		z-index: 60;
		width: max-content;
		max-width: calc(100vw - 32px);
		transform: translateX(-50%);
		font-size: 0.95rem;
		animation: rise 0.25s ease-out;
	}

	.inner {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 8px 8px 18px;
		border: 1px solid var(--line);
		border-radius: 999px;
		background: var(--surface);
		box-shadow: var(--shadow);
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translate(-50%, 8px);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.banner {
			animation: none;
		}
	}

	.button {
		min-height: 34px;
		padding: 0 14px;
		font-size: 0.88rem;
	}

	.close {
		display: grid;
		place-items: center;
		width: 34px;
		height: 34px;
		border: 0;
		border-radius: 999px;
		background: transparent;
		color: var(--ink-2);
		cursor: pointer;
	}

	.close:hover {
		background: var(--bg-soft);
	}
</style>
