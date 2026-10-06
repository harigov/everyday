<script lang="ts">
  // A sender's initials on their own colour -- see `lib/avatar.ts` for the
  // rules and for why there are never photos. Decorative: the name is always
  // written beside it, so it is hidden from assistive technology rather than
  // read out as two stray letters.

  import { avatarColor, initialsOf } from '../lib/avatar'

  let {
    name = '',
    email = '',
    size = 36,
  }: {
    name?: string
    email?: string
    /** Edge length in CSS pixels. */
    size?: number
  } = $props()

  const initials = $derived(initialsOf(name, email))
  const color = $derived(avatarColor(email || name))
</script>

<span
  class="avatar"
  class:one={initials.length === 1}
  style:--avatar-size="{size}px"
  style:background={color}
  aria-hidden="true">{initials}</span
>

<style>
  .avatar {
    flex: none;
    display: grid;
    place-items: center;
    width: var(--avatar-size);
    height: var(--avatar-size);
    border-radius: 50%;
    color: #fff;
    font-size: calc(var(--avatar-size) * 0.38);
    font-weight: 600;
    letter-spacing: 0.02em;
    line-height: 1;
    user-select: none;
    /* A hairline of the same colour, darker: on the light theme's near-white
       a pale circle otherwise loses its edge. */
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.06);
  }
  /* A single initial reads small in a circle sized for two. */
  .avatar.one {
    font-size: calc(var(--avatar-size) * 0.44);
  }
</style>
