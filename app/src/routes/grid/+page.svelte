<script lang="ts">
	import { VList } from 'virtua/svelte';

	// Placeholder virtualized grid: proves `virtua` resolves and renders in
	// this Tauri + SvelteKit skeleton. Real library grid views (thumbnails,
	// medium-type filtering, etc.) come later — see SPEC.md Roadmap.
	const COLUMNS = 6;
	const ITEM_COUNT = 500;

	const items = Array.from({ length: ITEM_COUNT }, (_, i) => i);
	const rows = Array.from({ length: Math.ceil(ITEM_COUNT / COLUMNS) }, (_, r) =>
		items.slice(r * COLUMNS, r * COLUMNS + COLUMNS)
	);
</script>

<div class="flex h-screen flex-col">
	<h1 class="p-4 text-xl font-bold">Virtualized grid placeholder ({ITEM_COUNT} items)</h1>
	<div class="min-h-0 flex-1">
		<VList data={rows} getKey={(_, i) => i} style="height: 100%;">
			{#snippet children(row)}
				<div
					class="grid gap-2 p-2"
					style="grid-template-columns: repeat({COLUMNS}, minmax(0, 1fr));"
				>
					{#each row as item (item)}
						<div
							class="flex aspect-square items-center justify-center rounded-lg border border-gray-300 bg-gray-100 dark:border-gray-700 dark:bg-gray-800"
						>
							{item}
						</div>
					{/each}
				</div>
			{/snippet}
		</VList>
	</div>
</div>
