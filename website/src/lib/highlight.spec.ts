import { describe, expect, it } from 'vitest';
import { highlight } from './highlight';

describe('highlight', () => {
	it('escapes HTML everywhere', () => {
		const html = highlight('const a = "<script>"; // </pre>&', 'js');
		expect(html).not.toMatch(/<script>|<\/pre>/);
		expect(html).toContain('&lt;script&gt;');
		expect(html).toContain('&amp;');
	});

	it('marks tokens', () => {
		expect(highlight('let x = 1; // hi', 'rust')).toBe(
			'<span class="tok-keyword">let</span> x = <span class="tok-number">1</span>; <span class="tok-comment">// hi</span>'
		);
		expect(highlight('println!("a")', 'rust')).toContain('<span class="tok-macro">println!</span>');
		expect(highlight('docker run --rm x # note', 'sh')).toContain(
			'<span class="tok-flag">--rm</span>'
		);
	});

	it('marks C', () => {
		const html = highlight('pc_output out; /* x */ pc_member_poll(m, &out) != PC_NONE', 'c');
		expect(html).toContain('<span class="tok-type">pc_output</span>');
		expect(html).toContain('<span class="tok-comment">/* x */</span>');
		expect(html).toContain('<span class="tok-fn">pc_member_poll</span>');
		expect(html).toContain('<span class="tok-macro">PC_NONE</span>');
		expect(html).toContain('&amp;out');
	});

	it('leaves text as it is, escaped', () => {
		expect(highlight('a < b', 'text')).toBe('a &lt; b');
	});
});
