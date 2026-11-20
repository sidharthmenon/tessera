<!-- A Screen Management number field. A valid number is committed on change; anything else is
     never buffered: the field snaps back to `value` and says what it takes. -->
<script lang="ts">
  import { parseNum } from '$lib/logic';

  let { label, value, int = false, min, onset }: { label: string; value: number; int?: boolean; min?: number; onset: (v: number) => void } =
    $props();

  let error = $state('');
  // A new value from elsewhere (a drag) clears the message.
  $effect(() => {
    void value;
    error = '';
  });

  function onchange(e: Event & { currentTarget: HTMLInputElement }) {
    const v = parseNum(e.currentTarget.value, { int, min });
    if (typeof v === 'number') {
      error = '';
      if (v !== value) onset(v);
    } else {
      error = v;
      e.currentTarget.value = String(value);
    }
  }
</script>

<label class="num" class:bad={error} title={error}>
  <span>{label}</span>
  <input type="number" {min} step={int ? 1 : 'any'} {value} {onchange} />
  {#if error}<em>{error}</em>{/if}
</label>

<style>
  .num {
    display: inline-flex;
    flex-direction: column;
    gap: 1px;
    position: relative;
  }
  span {
    font-size: 10px;
    color: var(--muted);
  }
  input {
    width: 76px;
  }
  .bad input {
    border-color: var(--live);
    outline: 1px solid var(--live);
  }
  em {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 2;
    margin-top: 2px;
    padding: 1px 5px;
    white-space: nowrap;
    font-style: normal;
    font-size: 10px;
    color: #fff;
    background: var(--live);
    border-radius: 3px;
  }
</style>
