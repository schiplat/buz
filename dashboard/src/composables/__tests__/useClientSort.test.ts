import { describe, expect, it } from 'vitest'
import { ref } from 'vue'
import { useClientSort } from '../useClientSort'

describe('useClientSort', () => {
  it('sortIndicator returns ↑ / ↓ / ↕ glyphs only (never asc/desc text)', () => {
    const rows = ref([{ name: 'b' }, { name: 'a' }])
    const { toggleSort, sortIndicator, sorted } = useClientSort(rows, {
      initialKey: 'name',
      initialDir: 'asc',
      getValue: (row, key) => (key === 'name' ? row.name : ''),
    })

    expect(sortIndicator('name')).toBe('↑')
    expect(sortIndicator('other')).toBe('↕')
    expect(sorted.value.map((r) => r.name)).toEqual(['a', 'b'])

    toggleSort('name')
    expect(sortIndicator('name')).toBe('↓')
    expect(sorted.value.map((r) => r.name)).toEqual(['b', 'a'])

    for (const glyph of [sortIndicator('name'), sortIndicator('other')]) {
      expect(['↑', '↓', '↕']).toContain(glyph)
      expect(glyph).not.toMatch(/asc|desc/i)
    }
  })
})
