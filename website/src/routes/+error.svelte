<script lang="ts">
	import { page } from '$app/state';
	import Seo from '#lib/components/Seo.svelte';
	import { i18n } from '#lib/i18n/current.svelte.ts';

	const t = $derived(i18n.t);
</script>

<Seo
	title={t.meta.notFound.title}
	description={t.meta.notFound.description}
	path={i18n.path}
	lang={i18n.lang}
	noindex
/>

<section class="container error">
	<p class="eyebrow">{page.status}</p>
	<h1>{t.notFound.title}</h1>
	<p>{t.notFound.text}</p>
	<a class="button primary" href={i18n.href('/')}>{t.notFound.home}</a>
</section>

<style>
	.error {
		display: grid;
		justify-items: start;
		gap: 16px;
		padding-block: clamp(80px, 16vw, 180px);
	}

	h1 {
		font-size: clamp(2rem, 5vw, 3rem);
	}

	p:not(.eyebrow) {
		color: var(--muted);
	}
</style>
