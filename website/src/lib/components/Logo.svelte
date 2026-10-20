<script lang="ts">
	let { size = 28 }: { size?: number } = $props();
</script>

<!-- Two peers, and the relay between them. -->
<svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true" class="logo">
	<rect width="32" height="32" rx="9" fill="var(--accent)" />
	<circle cx="9.5" cy="16" r="4.25" fill="var(--accent-ink)" />
	<circle cx="22.5" cy="16" r="4.25" fill="var(--accent-ink)" />
	<circle cx="16" cy="16" r="1.4" fill="var(--accent-ink)" fill-opacity="0.75" />
</svg>

<style>
	.logo {
		flex: none;
	}
</style>
