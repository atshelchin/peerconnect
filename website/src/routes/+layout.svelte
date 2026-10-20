<script lang="ts">
	import '../app.css';
	import Footer from '#lib/components/Footer.svelte';
	import Header from '#lib/components/Header.svelte';
	import LangBanner from '#lib/components/LangBanner.svelte';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<div class="page">
	<LangBanner />
	<Header />
	<main id="main">
		{@render children()}
	</main>
	<Footer />
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		min-height: 100vh;
		min-height: 100dvh;
	}

	main {
		flex: 1;
	}
</style>
