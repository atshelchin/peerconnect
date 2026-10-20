<script lang="ts">
	import { LANGS, LOCALE, localize, type Lang } from '#lib/i18n/index.ts';
	import { SITE_URL } from '#lib/site.ts';

	interface Props {
		title: string;
		description: string;
		/** The page's path without a language prefix, e.g. `/docs`. */
		path: string;
		lang: Lang;
		/** Whether the page exists in every language (otherwise English only). */
		translated?: boolean;
		type?: 'website' | 'article';
		jsonLd?: object[];
		noindex?: boolean;
	}

	let {
		title,
		description,
		path,
		lang,
		translated = true,
		type = 'website',
		jsonLd = [],
		noindex = false
	}: Props = $props();

	const absolute = (p: string) => SITE_URL + (p === '/' ? '/' : p);
	const url = $derived(absolute(localize(path, lang)));
	const image = $derived(`${SITE_URL}/og/${lang}.png`);
	const alternates = $derived(
		translated
			? LANGS.map((l) => ({ hreflang: LOCALE[l].tag, href: absolute(localize(path, l)) }))
			: []
	);
	// Escape `<` so that no string can close the script element early.
	const ld = $derived(
		jsonLd.map(
			(data) =>
				`<script type="application/ld+json">${JSON.stringify(data).replace(/</g, '\\u003c')}</` +
				'script>'
		)
	);
</script>

<svelte:head>
	<title>{title}</title>
	<meta name="description" content={description} />
	{#if noindex}
		<meta name="robots" content="noindex" />
	{:else}
		<link rel="canonical" href={url} />
		{#each alternates as alternate (alternate.hreflang)}
			<link rel="alternate" hreflang={alternate.hreflang} href={alternate.href} />
		{/each}
		{#if translated}
			<link rel="alternate" hreflang="x-default" href={absolute(path)} />
		{/if}
	{/if}

	<meta property="og:type" content={type} />
	<meta property="og:site_name" content="PeerConnect" />
	<meta property="og:title" content={title} />
	<meta property="og:description" content={description} />
	<meta property="og:url" content={url} />
	<meta property="og:locale" content={LOCALE[lang].og} />
	{#if translated}
		{#each LANGS.filter((l) => l !== lang) as other (other)}
			<meta property="og:locale:alternate" content={LOCALE[other].og} />
		{/each}
	{/if}
	<meta property="og:image" content={image} />
	<meta property="og:image:width" content="1200" />
	<meta property="og:image:height" content="630" />
	<meta property="og:image:alt" content={title} />
	<meta name="twitter:card" content="summary_large_image" />
	<meta name="twitter:title" content={title} />
	<meta name="twitter:description" content={description} />
	<meta name="twitter:image" content={image} />

	{#each ld as script, i (i)}
		<!-- eslint-disable-next-line svelte/no-at-html-tags -- serialized JSON, `<` escaped -->
		{@html script}
	{/each}
</svelte:head>
