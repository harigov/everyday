<script lang="ts">
  // One amount of money, as every Money screen writes it.
  //
  // Money coming in is tinted green, a step towards the text colour so it
  // stays legible on both themes; money going out is the ordinary text
  // colour. Out is the commoner of the two by a long way -- most of any list
  // of transactions is spending -- and a column of red would make every
  // purchase read as an alarm. The sign is in the figure itself either way,
  // so the colour reinforces and never carries: nobody has to tell green
  // from grey to know which way the money went.
  //
  // Tabular figures, so a column of these lines up on the decimal point.

  import { formatMoney } from '../lib/money'

  interface Props {
    /** Minor units; negative is money out. */
    amount: number
    /** ISO 4217. */
    currency: string
    /** Write a `+` in front of money in. */
    sign?: boolean
    /** `$12.3k` rather than `$12,345.67`, for a sidebar or an axis. */
    compact?: boolean
    /** Tint money in. Off for a balance, which is not a direction. */
    tone?: boolean
    /** How loud: a list row's figure, or the one a detail pane leads with. */
    size?: 'sm' | 'base' | 'lg' | 'xl'
    /** Struck through: left out of reports. */
    struck?: boolean
  }

  const {
    amount,
    currency,
    sign = false,
    compact = false,
    tone = true,
    size = 'base',
    struck = false,
  }: Props = $props()

  const text = $derived(formatMoney(amount, currency, { sign, compact }))
</script>

<span
  class="amount {size}"
  class:in={tone && amount > 0}
  class:struck
  title={compact ? formatMoney(amount, currency, { sign }) : undefined}>{text}</span
>

<style>
  .amount {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    color: inherit;
  }
  /* The green of the "good" band in the theme, mixed towards the text
     colour so it is a tint rather than a traffic light. Both themes define
     it, so dark mode gets its lighter step for free. */
  .in {
    color: color-mix(in oklab, var(--aqi-1) 82%, var(--fg));
  }
  .struck {
    text-decoration: line-through;
    text-decoration-color: var(--fg-faint);
  }

  .sm {
    font-size: var(--text-sm);
  }
  .base {
    font-size: var(--text-base);
  }
  .lg {
    font-size: var(--text-lg);
    font-weight: 600;
  }
  /* A lone figure at the top of a pane: proportional digits, as `StatTile`
     explains -- tabular gives "1" a zero's width, which looks loose at this
     size and matters only in a column. */
  .xl {
    font-size: var(--text-2xl);
    font-weight: 600;
    letter-spacing: -0.01em;
    font-variant-numeric: normal;
  }
</style>
