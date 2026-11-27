// Young Jatayu itself: a small three.js scene, built and animated in code.
//
// Built rather than downloaded. What this bird has to do is peer about, type,
// tilt its head, raise a wing and fall asleep, in any of five plumages, and
// that is a rig nobody has published -- so it is made of three.js's own
// spheres, capsules and lathes, every part a mesh of its own so a colour is a
// material on a part rather than a texture to paint.
//
// Jatayu is the great eagle of the Ramayana; this is him young. What makes
// him endearing is mostly not the shapes. He is drawn as a toy rather than
// an animal: a head bigger than the body, eyes wide apart and low, a small
// hooked beak, and down that is velvet -- a sheen that catches the light at
// the edges -- lit by a soft studio rather than a lamp, so he reads as
// something you could pick up. What makes him Jatayu is the wings: each is
// the application's icon, a stack of sickle feathers stepping from pale gold
// to deep, folded at his sides until there is a reason to open them. And he
// is alive in the small ways a toy is not: he blinks on his own clock, his
// crest lags a beat behind his head and wobbles when it stops, he watches
// the pointer, and he is pleased to be petted.
//
// The animation is poses, not keyframes. Each act is a function from time to
// a `Pose` -- a couple of dozen numbers: how far the body leans, where the
// head points, how high each wing is -- and moving from one act to the next
// blends from wherever the bird was to wherever the new act puts it, so an
// act that changes mid-flap does not snap. Everything that is not an act --
// the blinking, the crest's wobble, where it is looking, being petted -- is
// laid on top, so it carries on whatever the act.
//
// Loaded on demand (see `Companion.svelte`), so the rest of the interface
// does not carry three.js for a header that might be switched off.

import {
  BoxGeometry,
  CanvasTexture,
  CapsuleGeometry,
  Color,
  ConeGeometry,
  CylinderGeometry,
  DirectionalLight,
  ExtrudeGeometry,
  Group,
  HemisphereLight,
  LatheGeometry,
  Mesh,
  MeshBasicMaterial,
  MeshPhysicalMaterial,
  NeutralToneMapping,
  OctahedronGeometry,
  PCFShadowMap,
  PerspectiveCamera,
  PlaneGeometry,
  RepeatWrapping,
  ClampToEdgeWrapping,
  PMREMGenerator,
  Quaternion,
  Scene,
  Shape,
  SphereGeometry,
  SRGBColorSpace,
  TorusGeometry,
  Vector2,
  Vector3,
  WebGLRenderer,
  type BufferGeometry,
  type Material,
  type Object3D,
} from 'three'
import { RoomEnvironment } from 'three/examples/jsm/environments/RoomEnvironment.js'
import { parks, plumageOf, type Act, type Look } from './companion'

// ── The scene's public face ─────────────────────────────────────────────

export interface CompanionScene {
  setLook(look: Look): void
  setAct(act: Act): void
  /** The canvas's size in CSS pixels. */
  resize(width: number, height: number): void
  /**
   * Run the animation. A bird with nothing to do is drawn until it settles
   * and then held on that frame (see `parks`), until a new act, a pat or the
   * pointer moves it again.
   */
  play(): void
  /** Stop drawing, keeping everything for `play` to pick up. */
  pause(): void
  /**
   * Whether `play` was asked for and nothing has paused it since -- drawing,
   * or holding a settled frame that a pointer or a pat will wake.
   */
  readonly playing: boolean
  /**
   * Whether the window has the focus. A bird with nothing to do in a window
   * nobody is at is held as soon as it has come to rest (see `parks`); one at
   * work keeps moving, since that is how a turn shows from across the room.
   */
  attend(focused: boolean): void
  /**
   * Draw the current act once, standing still: for reduced motion. `at` is
   * how many seconds into the act to draw it -- far enough, left alone, that
   * the act has arrived.
   */
  still(at?: number): void
  /**
   * Where the pointer is from the bird, roughly -1 to 1 each way (right and
   * down are positive). It looks there for a while, then loses interest.
   */
  lookAt(x: number, y: number): void
  /** A pat: hearts, a wiggle, happy eyes, a chirp. */
  pet(): void
  dispose(): void
}

export interface CompanionOptions {
  look: Look
  act: Act
  /** For the thought, the question mark and the Zs: the app's accent. */
  accent: string
}

/** How long an act may idle before the bird settles down for a nap. */
const DOZE_AFTER_S = 180
/** How long a change of act takes to blend through. */
const BLEND_S = 0.45
/** Frames a second. A header ornament has no business asking for sixty. */
const FPS = 30
/**
 * Asleep, half that: it is only breathing, and a rail left open all day on
 * an assistant nobody has set up is a bird asleep all day.
 */
const SLEEP_FPS = 15
/** Which way the bird faces at rest: a touch to one side, so it reads as 3D. */
const FACING = 0.24
/** How long a pat lasts. */
const PET_S = 1.7
/** How long the pointer holds its attention after it stops moving. */
const ATTENTION_S = 3.5

/**
 * Make the scene, or `null` where WebGL is not available -- a webview with
 * hardware acceleration switched off, a context the browser refused. The
 * header then has no bird, which is the honest failure: nothing else depends
 * on it.
 */
export function createCompanionScene(
  canvas: HTMLCanvasElement,
  options: CompanionOptions,
): CompanionScene | null {
  const renderer = makeRenderer(canvas)
  if (!renderer) return null
  const scene = studio(renderer)
  const camera = new PerspectiveCamera(24, 1, 0.1, 40)

  const props = buildProps(accentOf(options.accent))
  scene.add(props.group)

  let shown = options.look
  let rig = buildBird(shown)
  scene.add(rig.root)

  let act: Act = options.act
  /** What is being acted out: `act`, unless an idle bird has nodded off. */
  let current: Act = act
  let actStart = 0
  let from: Pose | null = null
  let blend = 1
  let pose = poseFor(act, 0, 0)
  let blink = new Blinker()
  const life = new Life()

  let clock = 0
  let last = 0
  let frame = 0
  let width = 0
  let height = 0
  /** `play` was asked for, and nothing has paused it since. */
  let wanted = false
  /** The window has the focus: see `attend`. */
  let watched = true
  /** Settled with nothing to do: the loop has stopped on a finished frame. */
  let parked = false
  /** When it was held, by `performance.now()`. */
  let parkedAt = 0
  /** A held idle bird's nap, still to come. */
  let doze: ReturnType<typeof setTimeout> | null = null
  /** When it last changed what it was acting out, on `clock`. */
  let stirredAt = 0
  /** How open the eyes were last drawn. */
  let eyes = 1

  function step(dt: number) {
    clock += dt
    const u = clock - actStart
    // Left alone long enough, an idle bird settles down. Any new act wakes it.
    const acting: Act = act === 'idle' && u > DOZE_AFTER_S ? 'sleep' : act
    if (acting !== current) {
      from = { ...pose }
      blend = 0
      current = acting
      stirredAt = clock
    }
    const target = poseFor(acting, clock, u)
    if (from && blend < 1) {
      blend = Math.min(1, blend + dt / BLEND_S)
      pose = mix(from, target, ease(blend))
    } else {
      pose = target
    }
    const live = life.over(pose, current, clock, dt)
    eyes = blink.open(clock, live)
    apply(rig, live, clock, eyes)
    life.settle(rig, live, dt)
    props.animate(clock, live, rig)
  }

  function draw() {
    renderer!.render(scene, camera)
  }

  // Posed before anything can draw it. A bird as it comes out of
  // `buildBird` has its wings straight up and every prop showing, and the
  // first `resize` draws whatever is there -- a frame ahead of the loop.
  step(0)

  function loop(ms: number) {
    frame = requestAnimationFrame(loop)
    if (last === 0) last = ms
    const elapsed = (ms - last) / 1000
    if (elapsed < 1 / (current === 'sleep' ? SLEEP_FPS : FPS) - 0.002) return
    last = ms
    // A tab that slept for a minute must not wake into a minute's blend.
    step(Math.min(elapsed, 0.1))
    draw()
    const settled = parks({
      act: current,
      blend,
      quietFor: clock - stirredAt,
      watched,
      busy: life.busy(clock) || props.busy(clock),
      resting: eyes >= 1 && (current !== 'idle' || fidget(clock) < 0.02),
    })
    if (settled) park()
  }

  /**
   * Stop the loop on the frame just drawn, until something moves it: a new
   * act, a pat, the pointer, the focus coming back. An idle bird still nods
   * off when it would have, so its nap is put on a timer.
   */
  function park() {
    cancelAnimationFrame(frame)
    frame = 0
    parked = true
    parkedAt = performance.now()
    if (act === 'idle' && current === 'idle') {
      const left = DOZE_AFTER_S - (clock - actStart)
      // A beat late, so it wakes past the moment rather than just short of it.
      doze = setTimeout(wake, Math.max(0, left) * 1000 + 250)
    }
  }

  /**
   * Let go of a held frame. The time it was held counts towards an idle
   * bird's nap -- it was on screen all along -- and moving the act's start
   * back moves nothing drawn: idle's pose is read from the scene's clock.
   */
  function unpark() {
    if (!parked) return
    parked = false
    if (doze) clearTimeout(doze)
    doze = null
    if (act === 'idle') actStart -= (performance.now() - parkedAt) / 1000
  }

  /** Start the loop again for something that moves, if it is wanted. */
  function wake() {
    if (!wanted || frame) return
    unpark()
    last = 0
    frame = requestAnimationFrame(loop)
  }

  /** Between frames, or with no loop running: bring the picture up to date. */
  function refresh() {
    if (frame) return
    step(0)
    draw()
  }

  return {
    setLook(look) {
      if (look.plumage === shown.plumage) return
      shown = look
      scene.remove(rig.root)
      dispose(rig.root)
      rig = buildBird(look)
      scene.add(rig.root)
      refresh()
    },
    setAct(next) {
      if (next === act) return
      // Before the act changes: a held frame's time belongs to the old one.
      unpark()
      act = next
      actStart = clock
      blink = new Blinker(clock)
      refresh()
      wake()
    },
    resize(w, h) {
      width = w
      height = h
      // Drawn at twice the size even on a 1x display: at this size the edge
      // of a feather is a handful of pixels, and supersampling is what keeps it
      // from crawling. The cost, at under 200 pixels square, is nothing.
      renderer.setPixelRatio(Math.min(3, Math.max(2, window.devicePixelRatio || 1)))
      renderer.setSize(w, h, false)
      frameCamera(camera, width, height)
      if (!frame) draw()
    },
    play() {
      wanted = true
      if (frame || parked) return
      last = 0
      frame = requestAnimationFrame(loop)
    },
    pause() {
      wanted = false
      unpark()
      cancelAnimationFrame(frame)
      frame = 0
    },
    get playing() {
      return wanted
    },
    attend(focused) {
      if (focused === watched) return
      watched = focused
      // Back in front of somebody: moving again, until it settles as a
      // watched bird does. Held already, if it has been quiet that long.
      if (focused) wake()
    },
    still(at = 1.2) {
      wanted = false
      unpark()
      cancelAnimationFrame(frame)
      frame = 0
      // Far enough into the act that it has arrived, and not blinking. A
      // pat still shows, as a happy face, for as long as it lasts.
      clock = Math.max(clock, actStart + at)
      from = null
      current = act
      const live = life.over(poseFor(act, clock, at), act, clock, 0, true)
      apply(rig, live, clock, 1)
      props.animate(clock, live, rig)
      draw()
    },
    lookAt(x, y) {
      life.look(x, y, clock)
      // Only a bird that turns its head to the pointer is woken by it.
      if (ATTENTIVE[current]) wake()
    },
    pet() {
      life.pet(clock)
      props.hearts(clock)
      refresh()
      wake()
    },
    dispose() {
      unpark()
      cancelAnimationFrame(frame)
      frame = 0
      dispose(scene)
      scene.environment?.dispose()
      renderer.dispose()
      // Handed back now rather than when the collector gets to it: a
      // browser allows a page a handful of live WebGL contexts, and the
      // settings preview and the header can come and go many times.
      renderer.forceContextLoss()
    },
  }
}

function makeRenderer(canvas: HTMLCanvasElement): WebGLRenderer | null {
  try {
    const renderer = new WebGLRenderer({
      canvas,
      alpha: true,
      antialias: true,
      powerPreference: 'low-power',
    })
    renderer.outputColorSpace = SRGBColorSpace
    // Neutral rather than filmic: gold plumage should come out gold, not
    // the orange a film curve would push it to.
    renderer.toneMapping = NeutralToneMapping
    renderer.setClearColor(0x000000, 0)
    // Soft shadows between its own parts -- the head's on the chest, a
    // wing's on the body -- are most of what makes it read as a solid thing
    // in a room rather than shapes laid on a page. One small map, from the
    // key light only.
    renderer.shadowMap.enabled = true
    renderer.shadowMap.type = PCFShadowMap
    return renderer
  } catch {
    return null
  }
}

/**
 * The room it is lit in: a soft studio for the down to pick up, a warm key
 * from the front left, and a cool rim behind that the velvet catches.
 */
function studio(renderer: WebGLRenderer): Scene {
  const scene = new Scene()
  const pmrem = new PMREMGenerator(renderer)
  const room = new RoomEnvironment()
  // Small: it only has to be soft, and at this size a sharper one is a
  // quarter of a second spent on reflections nobody can see.
  scene.environment = pmrem.fromScene(room, 0.04, 0.1, 100, { size: 64 }).texture
  scene.environmentIntensity = 0.48
  room.dispose()
  pmrem.dispose()

  // Lit the way a soft toy is photographed: a warm key from above left that
  // casts the only shadows, a fill from the other side so nothing goes
  // black, a cool rim behind that the velvet catches, and warm light
  // bounced up off the floor into the underside of the head.
  scene.add(new HemisphereLight(0xfff6ec, 0xc89a6a, 0.4))
  const key = new DirectionalLight(0xfff0dc, 2.05)
  key.position.set(-2.2, 3.4, 3.2)
  key.castShadow = true
  key.shadow.mapSize.set(512, 512)
  key.shadow.radius = 5
  key.shadow.blurSamples = 12
  key.shadow.bias = -0.0006
  key.shadow.normalBias = 0.012
  const box = key.shadow.camera
  box.left = box.bottom = -1.2
  box.right = box.top = 1.2
  box.near = 1
  box.far = 9
  scene.add(key)
  const fill = new DirectionalLight(0xffe9d6, 0.45)
  fill.position.set(3, 1.2, 2.4)
  scene.add(fill)
  const rim = new DirectionalLight(0xe4ecff, 1.9)
  rim.position.set(2.0, 2.6, -2.8)
  scene.add(rim)
  const bounce = new DirectionalLight(0xffc98a, 0.35)
  bounce.position.set(0, -2, 1.5)
  scene.add(bounce)
  return scene
}

/**
 * Fitted to a box around the bird and what floats about it, whatever the
 * canvas's shape: the header's and the settings preview's both show the
 * whole bird, wings up. `zoom` above 1 crops in.
 */
function frameCamera(camera: PerspectiveCamera, width: number, height: number, zoom = 1) {
  const aspect = width / Math.max(1, height)
  camera.aspect = aspect
  const halfH = 0.98 / zoom
  const halfW = 0.98 / zoom
  const fit = Math.max(halfH, halfW / aspect)
  const distance = fit / Math.tan((camera.fov * Math.PI) / 360)
  const y = zoom > 1 ? 0.9 : 0.72
  camera.position.set(0.04, y + 0.06 + distance * 0.2, distance)
  camera.lookAt(0.04, y, 0)
  camera.updateProjectionMatrix()
}

function accentOf(style: string): Color {
  const colour = new Color('#d0782f')
  try {
    if (style.trim()) colour.setStyle(style.trim())
  } catch {
    /* an accent three.js cannot read is drawn in the default */
  }
  return colour
}

function dispose(root: Object3D) {
  root.traverse((node) => {
    const mesh = node as Mesh
    if (mesh.geometry) mesh.geometry.dispose()
    const material = mesh.material as Material | Material[] | undefined
    for (const m of Array.isArray(material) ? material : material ? [material] : []) {
      const maps = m as MeshPhysicalMaterial
      for (const map of [maps.map, maps.bumpMap, maps.roughnessMap, maps.alphaMap]) map?.dispose()
      m.dispose()
    }
  })
}

// ── Poses ────────────────────────────────────────────────────────────────

/** Everything an act decides about the bird, at one moment. */
interface Pose {
  /** The whole bird, turned towards the laptop (+) or the viewer. */
  yaw: number
  /** The whole bird, shuffled sideways while it hunts about. */
  shift: number
  /** Up for a hop, down for settling onto the ground. */
  lift: number
  /** The body's height, for breathing and landing. */
  squash: number
  /** The body, leaning forward from the feet. */
  lean: number
  headPitch: number
  headYaw: number
  headRoll: number
  /** The crest: standing and fanned (+), laid flat back (-). */
  crest: number
  /** The crest, tipped to one side. */
  crestTilt: number
  tailWag: number
  /** 0 is down, 1 is up and proud. */
  tailLift: number
  /** How far each wing is raised: 0 folded at its side, 1 up and open. Below 0 it hangs. */
  wingL: number
  wingR: number
  /** How far each wing reaches forward and down, for the keys. */
  tapL: number
  tapR: number
  /** The feathers spread apart without the wing being lifted: a ruffle. */
  fan: number
  /** The wings drawn round the front, like a blanket. */
  wrap: number
  /** 1 is open, 0 shut. Blinking is applied on top. */
  eyes: number
  /** The eyes drawn as happy arcs, past one half. */
  happy: number
  /** The eyes drawn shut, asleep, past one half. */
  shut: number
  /** The beak, open: a chirp. */
  beak: number
  /** The brows: inner ends down, concentrating (+); inner ends up, soft and open (-). */
  brow: number
  laptop: number
  thought: number
  question: number
  zzz: number
  puff: number
  /** A drop of worry on the side of the head. */
  sweat: number
  /** A bubble at the beak, asleep. */
  bubble: number
  sparkle: number
}

function ease(x: number): number {
  return x * x * (3 - 2 * x)
}

function mix(a: Pose, b: Pose, k: number): Pose {
  const out = { ...b }
  for (const key of Object.keys(b) as (keyof Pose)[]) out[key] = a[key] + (b[key] - a[key]) * k
  return out
}

/** A smooth bump, `width` seconds long, once every `period`. */
function pulse(t: number, period: number, width: number, offset = 0): number {
  const p = (((t + offset) % period) + period) % period
  return p < width ? Math.sin((p / width) * Math.PI) : 0
}

/** The same number for the same slot of time, and a different one for the next. */
function hash(n: number): number {
  const x = Math.sin(n * 127.1 + 311.7) * 43758.5453
  return x - Math.floor(x)
}

/** How long each of idle's little things to do lasts, from one to the next. */
const IDLE_SLOT_S = 4.6
/** The share of those slots in which it just sits. */
const IDLE_SITS = 0.22

/**
 * How far into one of idle's little things the bird is at `t`, 0 to 1: 0
 * between two of them, and all through a slot where it just sits. Where an
 * idle bird can be held without stopping mid-stretch.
 */
function fidget(t: number): number {
  const slot = Math.floor(t / IDLE_SLOT_S)
  if (hash(slot) < IDLE_SITS) return 0
  return envelope(t - slot * IDLE_SLOT_S, IDLE_SLOT_S, 0.7)
}

/** Fades in over `edge` seconds at the start of a slot and out at its end. */
function envelope(local: number, length: number, edge: number): number {
  return ease(Math.max(0, Math.min(1, local / edge, (length - local) / edge)))
}

/** Perched, breathing, looking a little towards whoever is looking at it. */
function rest(t: number): Pose {
  const breath = Math.sin(t * 2.2)
  return {
    yaw: 0,
    shift: 0,
    lift: 0,
    squash: 1 + breath * 0.02,
    lean: 0,
    headPitch: 0.02 + breath * 0.015,
    headYaw: -0.1,
    headRoll: 0,
    crest: 0,
    crestTilt: 0,
    tailWag: Math.sin(t * 1.7) * 0.08,
    tailLift: 0.55,
    wingL: breath * 0.012,
    wingR: breath * 0.012,
    tapL: 0,
    tapR: 0,
    fan: 0,
    wrap: 0,
    eyes: 1,
    happy: 0,
    shut: 0,
    beak: 0,
    brow: -0.2,
    laptop: 0,
    thought: 0,
    question: 0,
    zzz: 0,
    puff: 0,
    sweat: 0,
    bubble: 0,
    sparkle: 0,
  }
}

/**
 * The pose for an act, `t` seconds into the scene and `u` into the act.
 *
 * Idle is the only one with a mind of its own: every few seconds it picks
 * something small to do -- tilt its head, look round, ruffle its feathers,
 * stretch a wing, bounce -- from a slot of time hashed to a number, so it is
 * never the same loop twice in a row and never needs a random generator to
 * remember anything.
 */
function poseFor(act: Act, t: number, u: number): Pose {
  const p = rest(t)
  switch (act) {
    case 'idle': {
      const LENGTH = IDLE_SLOT_S
      const slot = Math.floor(t / LENGTH)
      const r = hash(slot)
      const side = hash(slot + 17) < 0.5 ? -1 : 1
      const w = envelope(t - slot * LENGTH, LENGTH, 0.7)
      if (r < IDLE_SITS) {
        // Sit.
      } else if (r < 0.42) {
        // The quick, sideways tilt a bird gives something it is curious about.
        p.headRoll += 0.34 * side * w
        p.crestTilt = 0.5 * side * w
        p.crest += 0.25 * w
      } else if (r < 0.58) {
        p.headYaw += 0.5 * side * w
        p.headPitch -= 0.06 * w
      } else if (r < 0.74) {
        // A ruffle: every feather stands out and shakes, then settles.
        const shake = Math.sin(t * 22)
        p.fan = 0.75 * w
        p.wingL += 0.1 * w + shake * 0.035 * w
        p.wingR += 0.1 * w - shake * 0.035 * w
        p.crest += 0.7 * w
        p.tailWag = p.tailWag * (1 - w) + Math.sin(t * 17) * 0.4 * w
        p.squash += 0.035 * w
        p.eyes = 1 - 0.5 * w
      } else if (r < 0.88) {
        // One wing stretched right out, slowly, and folded away again.
        const reach = 0.72 * w
        if (side > 0) p.wingL = reach
        else p.wingR = reach
        p.headRoll += 0.12 * side * w
        p.headYaw += 0.22 * side * w
      } else {
        // A happy little bounce on the spot, wings out for balance.
        const bounce = Math.abs(Math.sin(t * 6))
        p.lift = bounce * 0.05 * w
        p.squash += (bounce - 0.5) * 0.05 * w
        p.headRoll += Math.sin(t * 6) * 0.08 * w
        p.wingL += (0.18 + bounce * 0.14) * w
        p.wingR += (0.18 + bounce * 0.14) * w
        p.crest += 0.4 * w
        p.tailWag = p.tailWag * (1 - w) + Math.sin(t * 12) * 0.3 * w
      }
      return p
    }
    case 'listen': {
      // Crest up, head cocked down towards where the words are being typed.
      p.headPitch = 0.16 + Math.sin(u * 1.1) * 0.03
      p.headRoll = 0.18 * Math.sin(u * 0.6)
      p.headYaw = -0.05
      p.lean = 0.05
      p.crest = 0.85
      p.crestTilt = 0.25 * Math.sin(u * 0.6)
      p.brow = -0.3
      p.tailWag = Math.sin(u * 7) * 0.16
      p.tailLift = 0.9
      return p
    }
    case 'think': {
      // Head up and to one side, crest askew, a wingtip to its chin; dots.
      p.headPitch = -0.2 + Math.sin(u * 0.9) * 0.04
      p.headRoll = 0.24 + Math.sin(u * 0.7) * 0.05
      p.headYaw = 0.05 + Math.sin(u * 0.5) * 0.12
      p.crest = 0.35
      p.crestTilt = -0.55
      p.brow = 0.2
      p.wingR = 0.3 + Math.sin(u * 1.3) * 0.03
      p.tapR = 0.34
      p.tailWag = Math.sin(t * 1.6) * 0.06
      p.thought = 1
      return p
    }
    case 'sniff': {
      // Turned side-on, so the beak can be seen going to the ground, and
      // working along it: a look, a peck, a look somewhere else.
      const peck = pulse(u, 0.92, 0.3)
      p.lean = 0.4
      p.headPitch = 0.34 + peck * 0.2 + Math.sin(u * 2.4) * 0.03
      p.headYaw = Math.sin(u * 1.5) * 0.3
      p.headRoll = Math.sin(u * 1.5) * -0.08
      p.shift = Math.sin(u * 0.75) * 0.08
      p.yaw = 0.78 + Math.sin(u * 0.75) * 0.1
      p.crest = 0.45
      p.wingL = 0.1
      p.wingR = 0.1
      p.tailWag = Math.sin(u * 8) * 0.2
      p.tailLift = 1
      p.puff = peck
      return p
    }
    case 'type': {
      // Turned to the laptop, wingtips tapping in turn, a pause now and then
      // to read what it wrote.
      const reading = pulse(u, 3.4, 0.8, 2.6)
      const typing = 1 - reading
      const k = u * 12
      p.yaw = 0.48
      p.lean = 0.12
      p.headPitch = 0.22 + Math.sin(u * 6) * 0.02 * typing - reading * 0.06
      p.headYaw = -0.05 + reading * 0.12
      p.headRoll = reading * 0.12
      p.tapL = 0.78 + 0.2 * Math.max(0, Math.sin(k)) * typing
      p.tapR = 0.78 + 0.2 * Math.max(0, Math.sin(k + Math.PI)) * typing
      p.crest = -0.1 + reading * 0.5
      p.tailWag = Math.sin(u * 2.2) * 0.08
      p.laptop = 1
      return p
    }
    case 'ask': {
      // Looking straight at you, head cocked, a wing up like a hand: well?
      p.headYaw = -0.2
      p.headRoll = -0.3 + Math.sin(u * 1.4) * 0.04
      p.headPitch = -0.06
      p.crest = 0.7
      p.crestTilt = -0.3
      p.brow = -0.45
      p.wingR = 0.96 + Math.sin(u * 5) * 0.06
      p.tailWag = Math.sin(u * 9) * 0.2
      p.question = 1
      return p
    }
    case 'cheer': {
      const hop = Math.abs(Math.sin(u * Math.PI * 2.3))
      const flap = Math.sin(u * Math.PI * 4.6)
      p.lift = hop * 0.12
      p.squash = 0.94 + hop * 0.1
      p.headYaw = -0.15
      p.headPitch = -0.1
      p.crest = 1
      p.wingL = 0.86 + flap * 0.14
      p.wingR = 0.86 + flap * 0.14
      p.tailWag = Math.sin(u * 19) * 0.4
      p.tailLift = 1
      p.happy = 1
      p.beak = 0.6 + 0.4 * hop
      p.brow = -0.35
      p.sparkle = 1
      return p
    }
    case 'droop': {
      p.lean = 0.07
      p.squash = 0.97 + Math.sin(t * 1.3) * 0.01
      p.headPitch = 0.34
      p.headYaw = -0.12
      p.crest = -1
      p.wingL = -0.7
      p.wingR = -0.7
      p.tailWag = 0
      p.tailLift = 0
      p.eyes = 0.55
      p.brow = -1
      p.sweat = 1
      return p
    }
    case 'sleep': {
      // Settled onto the ground, head turned and sunk into its own feathers,
      // wings drawn round, breathing slowly; a bubble at the beak, and the Zs.
      const breath = Math.sin(t * 1.25)
      p.lift = -0.07
      p.lean = 0.1
      p.squash = 0.9 + breath * 0.03
      p.headPitch = 0.3 + breath * 0.02
      p.headYaw = -0.34
      p.headRoll = 0.3
      p.wrap = 1
      p.wingL = 0
      p.wingR = 0
      p.fan = 0.2 + breath * 0.05
      p.crest = -0.75
      p.tailWag = 0
      p.tailLift = 0.1
      p.eyes = 0
      p.shut = 1
      p.brow = -0.15
      p.zzz = 1
      p.bubble = 0.5 + 0.5 * breath
      return p
    }
  }
}

/** Blinks every few seconds, now and then twice, and never mid-act-change. */
class Blinker {
  #next: number
  #double = false
  constructor(now = 0) {
    this.#next = now + 1.5 + Math.random() * 2
  }
  /** How open the eyes are, 0 to 1, as far as blinking goes. */
  open(t: number, pose: Pose): number {
    if (pose.happy > 0.5 || pose.shut > 0.5) return 1
    const since = t - this.#next
    if (since < 0) return 1
    const BLINK = 0.15
    if (since > BLINK) {
      // Scheduled from the blink just finished: two close together,
      // sometimes, the way a real one does.
      this.#double = !this.#double && Math.random() < 0.25
      this.#next = t + (this.#double ? 0.18 : 2.4 + Math.random() * 3.2)
      return 1
    }
    return 1 - Math.sin((since / BLINK) * Math.PI)
  }
}

/** How much a gaze at the pointer is allowed to move the head, per act. */
const ATTENTIVE: Partial<Record<Act, number>> = {
  idle: 1,
  listen: 0.45,
  ask: 0.6,
  cheer: 0.4,
  think: 0.25,
  droop: 0.3,
}

/**
 * What is laid over an act: where it is looking, and being petted -- and,
 * after the pose is on the bird, the crest catching up with the head.
 */
class Life {
  #look = new Vector2()
  #gaze = new Vector2()
  #lookedAt = -Infinity
  #petAt = -Infinity
  #crest = new Spring()
  #head = new Vector3()
  #lift = 0

  look(x: number, y: number, now: number) {
    this.#look.set(Math.max(-1, Math.min(1, x)), Math.max(-1, Math.min(1, y)))
    this.#lookedAt = now
  }

  pet(now: number) {
    this.#petAt = now
  }

  /**
   * Whether anything laid over the act is still moving: a pat, the head
   * following the pointer or easing back from it, the crest's wobble.
   */
  busy(t: number): boolean {
    return (
      t - this.#petAt < PET_S ||
      this.#gaze.lengthSq() > 1e-4 ||
      Math.abs(this.#crest.x) > 0.01 ||
      Math.abs(this.#crest.v) > 0.05
    )
  }

  /** The pose with attention and petting on it. `still` skips the easing. */
  over(pose: Pose, act: Act, t: number, dt: number, still = false): Pose {
    const p = { ...pose }
    // Interest in the pointer fades once it stops moving, and the head eases
    // after it rather than snapping to every twitch of the mouse.
    const interest = still ? 0 : t - this.#lookedAt < ATTENTION_S ? 1 : 0
    const target = this.#look.clone().multiplyScalar(interest * (ATTENTIVE[act] ?? 0))
    this.#gaze.lerp(target, Math.min(1, dt * 4))
    p.headYaw += this.#gaze.x * 0.55
    p.headPitch += this.#gaze.y * 0.3
    p.headRoll -= this.#gaze.x * 0.08

    const since = t - this.#petAt
    if (since >= 0 && since < PET_S) {
      const w = envelope(since, PET_S, 0.25)
      const wiggle = Math.sin(since * 13)
      p.happy = Math.max(p.happy, w)
      p.shut = p.shut * (1 - w)
      p.headRoll = p.headRoll * (1 - w) + wiggle * 0.18 * w
      p.headPitch = p.headPitch * (1 - w) - 0.08 * w
      // Under a hand the crest goes flat, and everything else fluffs up.
      p.crest = p.crest * (1 - w) - 0.6 * w
      p.fan = Math.max(p.fan, 0.6 * w)
      p.tailWag = p.tailWag * (1 - w) + Math.sin(since * 20) * 0.4 * w
      p.squash += Math.abs(wiggle) * 0.04 * w
      p.beak = Math.max(p.beak, 0.7 * w)
      p.brow *= 1 - w
      p.zzz *= 1 - w
      p.bubble *= 1 - w
    }
    return p
  }

  /**
   * The crest, a beat behind the head: a spring pushed by how fast the head
   * turned and how hard the body landed, so a nod makes it flick and a hop
   * makes it bounce.
   */
  settle(rig: Rig, pose: Pose, dt: number) {
    if (dt <= 0) return
    const head = new Vector3(pose.headPitch, pose.headYaw, pose.headRoll)
    const turn = head.clone().sub(this.#head).divideScalar(dt)
    const lift = (pose.lift - this.#lift) / dt
    this.#head.copy(head)
    this.#lift = pose.lift
    this.#crest.step(dt, turn.x * 0.06 - lift * 0.5 + (turn.z + turn.y) * 0.04)
    rig.crest.rotation.x += this.#crest.x * 0.9
    rig.crest.rotation.z += this.#crest.x * 0.5
  }
}

/** A damped spring, a little under-damped so it wobbles once. */
class Spring {
  x = 0
  v = 0
  step(dt: number, push: number) {
    const STIFF = 140
    const DAMP = 9
    this.v += (push * STIFF * 0.02 - STIFF * this.x - DAMP * this.v) * dt
    this.x += this.v * dt
    this.x = Math.max(-0.6, Math.min(0.6, this.x))
  }
}

// ── The bird ─────────────────────────────────────────────────────────────

/** One wing: the shoulder it turns about, and its feathers, front one first. */
interface Wing {
  arm: Group
  feathers: Mesh[]
}

/** The parts that move, and where they started. */
interface Rig {
  root: Group
  hips: Group
  chest: Group
  neck: Group
  neckY: number
  crest: Group
  crestFeathers: Mesh[]
  wings: [Wing, Wing]
  tail: Group
  tailFeathers: Mesh[]
  eyes: [Group, Group]
  brows: [Mesh, Mesh]
  happy: [Mesh, Mesh]
  shut: [Mesh, Mesh]
  /** The tip of the beak, for whatever comes off it. */
  beakTip: Object3D
  jaw: Group
  sweat: Group
  bubble: Mesh
  laptop: Group
  screen: CanvasTexture
}

const EYE = '#191415'
const BEAK = '#f6a31c'
const CERE = '#ffd76a'
const FEET = '#f39a1b'
const INSIDE = '#d9566a'

/**
 * Velvet: the down's sheen is what makes it read as a soft toy rather than a
 * painted figure -- a pale glow at the edges, where the light grazes it.
 */
function down(hex: string, surface?: Surface): MeshPhysicalMaterial {
  return new MeshPhysicalMaterial({
    color: hex,
    roughness: 0.8,
    metalness: 0,
    sheen: 0.9,
    sheenRoughness: 0.42,
    sheenColor: new Color(hex).lerp(new Color('#ffffff'), 0.45),
    map: surface?.map ?? null,
    bumpMap: surface?.bump ?? null,
    bumpScale: surface ? 0.6 : 1,
  })
}

/**
 * A flight feather: smoother than down, with a little of the lacquer the
 * icon's feathers have, so the wing reads as made of separate blades.
 */
function vane(hex: string, surface?: Surface): MeshPhysicalMaterial {
  return new MeshPhysicalMaterial({
    color: hex,
    roughness: 0.55,
    metalness: 0,
    clearcoat: 0.3,
    clearcoatRoughness: 0.45,
    sheen: 0.6,
    sheenRoughness: 0.45,
    sheenColor: new Color(hex).lerp(new Color('#ffffff'), 0.5),
    map: surface?.map ?? null,
    bumpMap: surface?.bump ?? null,
    bumpScale: surface ? 2 : 1,
  })
}

// ── Surfaces ─────────────────────────────────────────────────────────────
//
// Painted once per bird into small canvases, in grey, so one picture serves
// every colour: the material's colour tints it. What they add is what a
// smooth sphere lacks -- the grain of down and the vanes of a feather -- and
// they are kept faint, because from across the room it is still a toy.

/** A colour picture to tint and a height picture to light, for one surface. */
interface Surface {
  map: CanvasTexture
  bump: CanvasTexture
}

/** A pseudo-random number for `n`, the same every time: the bird never changes. */
function seeded(n: number): number {
  const x = Math.sin(n * 91.345 + 47.853) * 43758.5453
  return x - Math.floor(x)
}

function paper(
  width: number,
  height: number,
  fill: string,
): [HTMLCanvasElement, CanvasRenderingContext2D] {
  const canvas = document.createElement('canvas')
  canvas.width = width
  canvas.height = height
  const g = canvas.getContext('2d')!
  g.fillStyle = fill
  g.fillRect(0, 0, width, height)
  return [canvas, g]
}

function texture(canvas: HTMLCanvasElement, colour: boolean, repeat = 1): CanvasTexture {
  const t = new CanvasTexture(canvas)
  if (colour) t.colorSpace = SRGBColorSpace
  t.wrapS = t.wrapT = RepeatWrapping
  t.repeat.set(repeat, repeat)
  t.anisotropy = 4
  return t
}

/**
 * Down, as felt: a fine, even grain of fibres -- the nap of a tennis ball --
 * with no tufts or clumps in it, because at this size anything bigger than
 * a fibre reads as blemishes rather than fur. A faint, broad mottle under it
 * keeps a large surface from looking printed.
 */
function downSurface(): Surface {
  const S = 256
  const [colourCanvas, c] = paper(S, S, '#ffffff')
  const [bumpCanvas, b] = paper(S, S, '#808080')
  const colour = c.getImageData(0, 0, S, S)
  const bump = b.getImageData(0, 0, S, S)
  for (let y = 0; y < S; y++) {
    for (let x = 0; x < S; x++) {
      const i = (y * S + x) * 4
      const grain = seeded(x * 0.0137 + y * 1.731) - 0.5
      // Broad and soft: two waves that repeat with the tile.
      const mottle =
        Math.sin((x / S) * Math.PI * 4 + Math.sin((y / S) * Math.PI * 2) * 1.5) *
        Math.cos((y / S) * Math.PI * 6)
      const tone = 252 + grain * 2.5 + mottle * 2
      colour.data[i] = colour.data[i + 1] = colour.data[i + 2] = tone
      const height = 128 + grain * 10
      bump.data[i] = bump.data[i + 1] = bump.data[i + 2] = height
    }
  }
  c.putImageData(colour, 0, 0)
  b.putImageData(bump, 0, 0)
  return { map: texture(colourCanvas, true, 2), bump: texture(bumpCanvas, false, 4) }
}

/**
 * Felt fibres seen end on, for the shells: one texel per fibre, its
 * brightness how far out it reaches, so a shell `k` of the way out keeps
 * only the fibres at least that long -- which is how a stack of shells
 * becomes a nap that thins towards its tips. Fine and dense, so what shows
 * is a soft, even halo round the outline rather than separate strands.
 */
function strandTexture(): CanvasTexture {
  const S = 128
  const [canvas, g] = paper(S, S, '#000000')
  const image = g.getImageData(0, 0, S, S)
  for (let i = 0; i < S * S; i++) {
    // Most fibres are short; a few reach the whole way.
    const reach = Math.pow(seeded(i * 0.618 + 0.11), 0.7)
    image.data[i * 4] = image.data[i * 4 + 1] = image.data[i * 4 + 2] = Math.round(reach * 255)
  }
  g.putImageData(image, 0, 0)
  // Softened by a hair, so a fibre is a little round tip rather than a hard
  // square texel that twinkles as the bird moves. Drawn three by three so
  // the blur wraps round the tile's edges too.
  const [soft, h] = paper(S, S, '#000000')
  h.filter = 'blur(0.7px)'
  for (const ox of [-S, 0, S]) for (const oy of [-S, 0, S]) h.drawImage(canvas, ox, oy)
  const t = new CanvasTexture(soft)
  t.wrapS = t.wrapT = RepeatWrapping
  t.repeat.set(7, 5)
  return t
}

/** How many shells the fur is drawn in, and how far out the last one is. */
const FUR_SHELLS = 6
const FUR_LENGTH = 0.007

/**
 * Fur on a ball of down: thin shells round it, each a little further out,
 * each cut to the strands that reach it. It is what gives the head and body
 * a soft, fuzzy edge against whatever is behind them -- the thing a smooth
 * sphere can never have, however it is lit.
 *
 * Cut rather than blended (`alphaTest`, smoothed by `alphaToCoverage`), so
 * the shells need no sorting and cost no more than drawing the ball again
 * a few times; children of the ball, so they move, squash and turn with it.
 */
function furOn(ball: Mesh, hex: string, strands: CanvasTexture) {
  const geometry = ball.geometry as SphereGeometry
  const radius = geometry.parameters.radius
  for (let i = 1; i <= FUR_SHELLS; i++) {
    const k = i / FUR_SHELLS
    const out = FUR_LENGTH * k
    const shell = new Mesh(
      geometry,
      new MeshPhysicalMaterial({
        // The same velvet as the down under it, so a fibre differs from the
        // surface only in standing a hair proud of it. A shell lit any other
        // way shows every fibre as a dot of another colour -- glitter.
        color: hex,
        roughness: 0.8,
        sheen: 0.9,
        sheenRoughness: 0.42,
        sheenColor: new Color(hex).lerp(new Color('#ffffff'), 0.45),
        alphaMap: strands,
        // Nearly whole close in, which reads as velvet; sparse only at the
        // tips, where the fibres soften the outline. Half-covered shells
        // all the way out are what read as grain.
        alphaTest: 0.04 + 0.86 * k * k,
        alphaToCoverage: true,
      }),
    )
    // Each axis grown by the same distance, whatever the ball's proportions.
    shell.scale.set(
      1 + out / (radius * ball.scale.x),
      1 + out / (radius * ball.scale.y),
      1 + out / (radius * ball.scale.z),
    )
    shell.userData.fur = true
    ball.add(shell)
  }
}

/**
 * A flight feather's vanes: a pale shaft down its middle, following the same
 * curve the feather does, fine barbs sweeping from it towards the tip, and a
 * little shade at the root where it tucks under the one in front.
 *
 * Laid out in the feather's own unit square -- across is `u`, root to tip
 * is `v` -- which `featherGeometry` maps its faces onto. `spine` says where
 * across the square the shaft is, at each point along it.
 */
function featherSurface(spine: (k: number) => number): Surface {
  const W = 128
  const H = 256
  const [colourCanvas, c] = paper(W, H, '#ffffff')
  const [bumpCanvas, b] = paper(W, H, '#808080')
  // Darker towards the root, the way a stack of real ones is.
  const shade = c.createLinearGradient(0, 0, 0, H)
  shade.addColorStop(0, 'rgba(255, 255, 255, 0)')
  shade.addColorStop(0.7, 'rgba(70, 40, 10, 0.05)')
  shade.addColorStop(1, 'rgba(70, 40, 10, 0.22)')
  c.fillStyle = shade
  c.fillRect(0, 0, W, H)
  c.lineCap = b.lineCap = 'round'
  // Canvas rows run from the tip (top) to the root (bottom).
  const at = (k: number) => [spine(k) * W, (1 - k) * H] as const
  for (let i = 0; i < 70; i++) {
    const k = 0.04 + (i / 70) * 0.92
    const [x, y] = at(k)
    for (const side of [-1, 1]) {
      const reach = W * 0.5
      const barb = (g: CanvasRenderingContext2D, style: string, width: number, shift: number) => {
        g.strokeStyle = style
        g.lineWidth = width
        g.beginPath()
        g.moveTo(x, y + shift)
        g.quadraticCurveTo(
          x + side * reach * 0.4,
          y - reach * 0.12 + shift,
          x + side * reach,
          y - reach * 0.55 + shift,
        )
        g.stroke()
      }
      barb(c, 'rgba(70, 40, 10, 0.1)', 1.4, 0)
      barb(b, 'rgba(0, 0, 0, 0.3)', 1.6, 0.8)
      barb(b, 'rgba(255, 255, 255, 0.22)', 1, -0.6)
    }
  }
  // The shaft: a pale line, raised, from the root almost to the tip.
  const shaft = (g: CanvasRenderingContext2D, style: string, width: number) => {
    g.strokeStyle = style
    g.lineWidth = width
    g.beginPath()
    for (let i = 0; i <= 40; i++) {
      const [x, y] = at((i / 40) * 0.9)
      if (i === 0) g.moveTo(x, y)
      else g.lineTo(x, y)
    }
    g.stroke()
  }
  shaft(c, 'rgba(255, 255, 255, 0.9)', 3.5)
  shaft(b, 'rgba(255, 255, 255, 0.5)', 4)
  const map = texture(colourCanvas, true)
  const bump = texture(bumpCanvas, false)
  for (const t of [map, bump]) t.wrapS = t.wrapT = ClampToEdgeWrapping
  return { map, bump }
}

/**
 * An eye, seen from the front, painted at a size that stays sharp however
 * close the camera is: a big round pupil; an iris of fine radial fibres,
 * dark at the pupil and warmer towards its edge; a darker rim round it; a
 * crescent of light glowing up through the lower iris, the way it does in a
 * real one lit from above; and the dark of the eye round all of that. No
 * catchlight is painted: those are separate, so they stay where the light
 * is whichever way the eye turns.
 */
function eyeTexture(): CanvasTexture {
  const S = 512
  const C = S / 2
  const [canvas, g] = paper(S, S, '#140d0a')
  const IRIS = S * 0.43
  // Large, for a young and gentle look; a small pupil in a big iris stares.
  const PUPIL = S * 0.28
  // The iris, warm brown, darker in towards the pupil.
  const base = g.createRadialGradient(C, C, PUPIL * 0.8, C, C, IRIS)
  base.addColorStop(0, '#2a140a')
  base.addColorStop(0.55, '#5e3215')
  base.addColorStop(0.9, '#7a4519')
  base.addColorStop(1, '#4a260f')
  g.fillStyle = base
  g.beginPath()
  g.arc(C, C, IRIS, 0, Math.PI * 2)
  g.fill()
  // Its fibres: fine strokes from the pupil out, lighter and darker by turns.
  g.lineCap = 'round'
  for (let i = 0; i < 360; i++) {
    const a = (i / 360) * Math.PI * 2 + (seeded(i + 0.4) - 0.5) * 0.02
    const r0 = PUPIL * (0.95 + seeded(i + 0.2) * 0.15)
    const r1 = IRIS * (0.78 + seeded(i + 0.9) * 0.2)
    const light = seeded(i + 0.66) > 0.5
    g.strokeStyle = light ? 'rgba(214, 150, 80, 0.22)' : 'rgba(20, 8, 2, 0.28)'
    g.lineWidth = 1 + seeded(i + 0.33) * 1.6
    g.beginPath()
    g.moveTo(C + Math.cos(a) * r0, C + Math.sin(a) * r0)
    g.lineTo(C + Math.cos(a) * r1, C + Math.sin(a) * r1)
    g.stroke()
  }
  // Light coming up through the lower half: a soft warm crescent.
  const glow = g.createRadialGradient(
    C,
    C + IRIS * 0.55,
    IRIS * 0.1,
    C,
    C + IRIS * 0.4,
    IRIS * 0.75,
  )
  glow.addColorStop(0, 'rgba(232, 160, 80, 0.55)')
  glow.addColorStop(1, 'rgba(232, 160, 80, 0)')
  g.save()
  g.beginPath()
  g.arc(C, C, IRIS, 0, Math.PI * 2)
  g.clip()
  g.fillStyle = glow
  g.fillRect(0, 0, S, S)
  // And the top of the iris in the shade of the lid.
  const lid = g.createLinearGradient(0, C - IRIS, 0, C)
  lid.addColorStop(0, 'rgba(10, 4, 2, 0.5)')
  lid.addColorStop(1, 'rgba(10, 4, 2, 0)')
  g.fillStyle = lid
  g.fillRect(0, 0, S, S)
  g.restore()
  // The dark rim round the iris, soft on both sides.
  const rim = g.createRadialGradient(C, C, IRIS * 0.86, C, C, IRIS * 1.04)
  rim.addColorStop(0, 'rgba(18, 9, 5, 0)')
  rim.addColorStop(0.6, 'rgba(18, 9, 5, 0.85)')
  rim.addColorStop(1, 'rgba(20, 13, 10, 1)')
  g.fillStyle = rim
  g.beginPath()
  g.arc(C, C, IRIS * 1.06, 0, Math.PI * 2)
  g.fill()
  // The pupil, with an edge a fraction soft so it is not cut out of paper.
  const pupil = g.createRadialGradient(C, C, PUPIL * 0.9, C, C, PUPIL * 1.06)
  pupil.addColorStop(0, '#040303')
  pupil.addColorStop(1, 'rgba(4, 3, 3, 0)')
  g.fillStyle = pupil
  g.beginPath()
  g.arc(C, C, PUPIL * 1.06, 0, Math.PI * 2)
  g.fill()
  const t = new CanvasTexture(canvas)
  t.colorSpace = SRGBColorSpace
  t.anisotropy = 8
  return t
}

/**
 * A ball for an eye, its front painted by looking straight at it: each point
 * takes the picture at its own x and y, so the pupil is perfectly round and
 * sharp. (A sphere's own mapping gathers to a point in the middle of the
 * front, which is exactly where the pupil goes.)
 */
function eyeGeometry(radius: number): BufferGeometry {
  const geometry = new SphereGeometry(radius, 64, 48)
  const position = geometry.getAttribute('position')
  const uv = geometry.getAttribute('uv')
  for (let i = 0; i < position.count; i++) {
    uv.setXY(i, 0.5 + position.getX(i) / (2 * radius), 0.5 + position.getY(i) / (2 * radius))
  }
  uv.needsUpdate = true
  return geometry
}

/** Wet and glossy, with a clear coat: eyes, and the beak. */
function glossy(hex: string, roughness = 0.25): MeshPhysicalMaterial {
  return new MeshPhysicalMaterial({
    color: hex,
    roughness,
    metalness: 0,
    clearcoat: 1,
    clearcoatRoughness: 0.06,
  })
}

function sphere(radius: number, material: Material, segments = 28): Mesh {
  return new Mesh(new SphereGeometry(radius, segments, Math.round(segments * 0.75)), material)
}

function place<T extends Object3D>(
  mesh: T,
  x: number,
  y: number,
  z: number,
  sx = 1,
  sy = 1,
  sz = 1,
): T {
  mesh.position.set(x, y, z)
  mesh.scale.set(sx, sy, sz)
  return mesh
}

/** The colour `k` of the way from one to another, as a hex string. */
function between(from: string, to: string, k: number): string {
  return `#${new Color(from).lerp(new Color(to), k).getHexString()}`
}

/** Is this plumage light enough that a line drawn on it should be dark? */
function isLight(hex: string): boolean {
  const n = parseInt(hex.slice(1), 16)
  const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255]
  return (r * 0.3 + g * 0.59 + b * 0.11) / 255 > 0.5
}

/**
 * A point on an ellipsoid's surface, and which way is out, for laying an eye
 * or a cheek onto a head that is not a sphere.
 */
function onEllipsoid(
  centre: Vector3,
  radii: Vector3,
  theta: number,
  phi: number,
): { at: Vector3; normal: Vector3 } {
  const dir = new Vector3(
    Math.cos(phi) * Math.sin(theta),
    Math.sin(phi),
    Math.cos(phi) * Math.cos(theta),
  )
  const at = new Vector3(dir.x * radii.x, dir.y * radii.y, dir.z * radii.z).add(centre)
  const normal = new Vector3(dir.x / radii.x, dir.y / radii.y, dir.z / radii.z).normalize()
  return { at, normal }
}

const FORWARD = new Vector3(0, 0, 1)

/** Turn an object so its +z points along `normal`. */
function facing<T extends Object3D>(mesh: T, normal: Vector3): T {
  mesh.quaternion.copy(new Quaternion().setFromUnitVectors(FORWARD, normal))
  return mesh
}

/** A soft round glow, for blush: a gradient rather than a disc. */
function glowTexture(colour: string): CanvasTexture {
  const canvas = document.createElement('canvas')
  canvas.width = canvas.height = 64
  const g = canvas.getContext('2d')!
  const gradient = g.createRadialGradient(32, 32, 0, 32, 32, 32)
  gradient.addColorStop(0, colour)
  gradient.addColorStop(1, 'rgba(255, 140, 150, 0)')
  g.fillStyle = gradient
  g.fillRect(0, 0, 64, 64)
  const texture = new CanvasTexture(canvas)
  texture.colorSpace = SRGBColorSpace
  return texture
}

/**
 * One feather, a unit long: the sickle the application's icon is made of,
 * cut from a sheet and softened at its edges.
 *
 * It runs from its root at the origin to its tip at (0, 1), bowed towards
 * +x, in the xy plane, so a wing is this one shape turned about its root and
 * shrunk a little each time -- which is exactly how the icon is drawn. The
 * root comes to a point, because a stack of them has to close to one; the
 * tip is round, because nothing sharp is cute.
 */
function featherGeometry(): { geometry: BufferGeometry; surface: Surface } {
  const STEPS = 30
  const BOW = 0.19
  const HALF = 0.125
  // The circle through the root and the tip whose arc stands BOW off the
  // straight line between them.
  const radius = (0.25 + BOW * BOW) / (2 * BOW)
  const centreX = BOW - radius
  const sweep = Math.asin(0.5 / radius)
  const outer: Vector2[] = []
  const inner: Vector2[] = []
  for (let i = 0; i <= STEPS; i++) {
    const k = i / STEPS
    const angle = -sweep + 2 * sweep * k
    const out = new Vector2(Math.cos(angle), Math.sin(angle))
    const spine = new Vector2(centreX + radius * out.x, 0.5 + radius * out.y)
    // Widest a little past the middle; a point at the root, a curve at the tip.
    const width = HALF * Math.pow(Math.sin(Math.PI * Math.pow(k, 0.9)), k < 0.55 ? 0.85 : 0.5)
    outer.push(spine.clone().addScaledVector(out, width))
    inner.push(spine.clone().addScaledVector(out, -width))
  }
  const shape = new Shape([...outer, ...inner.reverse().slice(1, -1)])
  const DEPTH = 0.03
  const geometry = new ExtrudeGeometry(shape, {
    depth: DEPTH,
    bevelEnabled: true,
    bevelThickness: 0.022,
    bevelSize: 0.02,
    bevelSegments: 4,
    curveSegments: 1,
  })
  geometry.translate(0, 0, -DEPTH / 2)
  // Faces mapped onto the unit square across and along the feather, so its
  // vanes can be painted once in `featherSurface`.
  const xs = [...inner, ...outer].map((p) => p.x)
  const minX = Math.min(...xs) - 0.02
  const maxX = Math.max(...xs) + 0.02
  const position = geometry.getAttribute('position')
  const uv = geometry.getAttribute('uv')
  for (let i = 0; i < position.count; i++) {
    const v = Math.max(0, Math.min(1, position.getY(i)))
    uv.setXY(i, (position.getX(i) - minX) / (maxX - minX), v)
  }
  uv.needsUpdate = true
  const spine = (k: number) => {
    const angle = -sweep + 2 * sweep * k
    return (centreX + radius * Math.cos(angle) - minX) / (maxX - minX)
  }
  return { geometry, surface: featherSurface(spine) }
}

/**
 * A beak: a cone with a rounded tip, laid forward along +z and bent down
 * over its length, so even a short one is unmistakably hooked.
 */
function beakGeometry(radius: number, length: number, droop: number): BufferGeometry {
  const points: Vector2[] = []
  const STEPS = 14
  for (let i = 0; i <= STEPS; i++) {
    const k = i / STEPS
    const r =
      radius *
      Math.pow(1 - k, 0.62) *
      (k > 0.8 ? Math.sqrt(Math.max(0, 1 - ((k - 0.8) / 0.2) ** 2)) : 1)
    points.push(new Vector2(Math.max(0.0001, r), length * k))
  }
  const geometry = new LatheGeometry(points, 24)
  geometry.rotateX(Math.PI / 2)
  const position = geometry.getAttribute('position')
  for (let i = 0; i < position.count; i++) {
    const k = Math.max(0, Math.min(1, position.getZ(i) / length))
    position.setY(i, position.getY(i) - droop * Math.pow(k, 2.6))
  }
  position.needsUpdate = true
  geometry.computeVertexNormals()
  return geometry
}

function heartGeometry(): BufferGeometry {
  const s = new Shape()
  s.moveTo(0, -0.5)
  s.bezierCurveTo(-0.15, -0.36, -0.56, -0.12, -0.5, 0.16)
  s.bezierCurveTo(-0.44, 0.46, -0.08, 0.5, 0, 0.24)
  s.bezierCurveTo(0.08, 0.5, 0.44, 0.46, 0.5, 0.16)
  s.bezierCurveTo(0.56, -0.12, 0.15, -0.36, 0, -0.5)
  const geometry = new ExtrudeGeometry(s, {
    depth: 0.16,
    bevelEnabled: true,
    bevelThickness: 0.12,
    bevelSize: 0.1,
    bevelSegments: 4,
    curveSegments: 18,
  })
  geometry.center()
  return geometry
}

/** How many feathers a wing has, and how each one shrinks from the last. */
const WING_FEATHERS = 5
const WING_SHRINK = 0.86
/** The first feather's length, and how far it stands off straight up, raised. */
const WING_LENGTH = 0.66
const WING_LEAD = 0.42
/** A folded wing is a shorter one: the feathers slide over each other as it closes. */
const WING_FOLDED = 0.6
/** The angle between one feather and the next: folded, and fully open. */
const WING_CLOSED = 0.16
const WING_OPEN = 0.4

/**
 * A fan of feathers about one point, the first in front: a wing, the crest,
 * the tail. Each is a step further along `tones` and a little further back,
 * so they overlap like the icon's.
 */
function fan(
  geometry: BufferGeometry,
  count: number,
  length: number,
  shrink: number,
  tones: [string, string],
  surface?: Surface,
): Mesh[] {
  const feathers: Mesh[] = []
  for (let i = 0; i < count; i++) {
    const size = length * Math.pow(shrink, i)
    const feather = new Mesh(
      geometry,
      vane(between(tones[0], tones[1], i / Math.max(1, count - 1)), surface),
    )
    feather.scale.set(size, size, 0.62)
    feather.position.z = -i * 0.017
    feathers.push(feather)
  }
  return feathers
}

function buildBird(look: Look): Rig {
  const plumage = plumageOf(look.plumage)
  const downy = downSurface()
  const base = down(plumage.base, downy)
  const pale = down(plumage.pale, downy)
  const beakMat = glossy(BEAK, 0.34)
  const feetMat = glossy(FEET, 0.5)
  const { geometry: feather, surface: vanes } = featherGeometry()

  const root = new Group()
  root.rotation.y = FACING
  const hips = new Group()
  root.add(hips)

  // ---- body: a small, round chick's body, mostly hidden behind a big head ---
  const chest = new Group()
  hips.add(chest)
  chest.add(place(sphere(1, base, 36), 0, 0.33, -0.03, 0.34, 0.34, 0.31))
  // The bib: the pale down of the chest, and its soft swell.
  chest.add(place(sphere(1, pale, 28), 0, 0.35, 0.115, 0.235, 0.265, 0.19))

  // Feathered thighs, and feet: three round toes each, the colour of the beak.
  for (const side of [-1, 1]) {
    hips.add(place(sphere(0.15, base), side * 0.17, 0.15, 0.0, 0.85, 0.95, 1.05))
    const foot = new Group()
    foot.position.set(side * 0.155, 0.032, 0.17)
    foot.rotation.y = side * 0.16
    for (const splay of [-0.5, 0, 0.5]) {
      const toe = new Mesh(new CapsuleGeometry(0.03, 0.09, 6, 12), feetMat)
      toe.rotation.set(Math.PI / 2, 0, -splay)
      toe.position.set(Math.sin(splay) * 0.06, 0, Math.cos(splay) * 0.06)
      foot.add(toe)
    }
    foot.add(place(sphere(0.045, feetMat, 14), 0, 0.004, -0.005, 1, 0.75, 1))
    hips.add(foot)
  }

  // ---- tail: three feathers fanned out behind ------------------------------
  const tail = new Group()
  tail.position.set(0, 0.2, -0.27)
  hips.add(tail)
  const tailFeathers = fan(feather, 3, 0.36, 0.9, [plumage.deep, plumage.wing[1]], vanes)
  tailFeathers.forEach((f, i) => {
    // Middle one on top, the others either side of it, mirrored.
    const side = i === 0 ? 0 : i === 1 ? -1 : 1
    if (side !== 0) f.scale.x *= side
    f.rotation.z = -side * 0.5 + (side === 0 ? 0.18 : 0)
    f.position.z = -Math.abs(side) * 0.012
    tail.add(f)
  })

  // ---- wings: the icon, one each side -----------------------------------------
  //
  // Built for the wing that is raised to the viewer's right, in a frame
  // where +x is "away from the body"; the other is the same thing mirrored
  // by its parent, so one set of angles moves both.
  const wings: Wing[] = []
  for (const side of [-1, 1]) {
    const shoulder = new Group()
    shoulder.position.set(side * 0.33, 0.44, 0.03)
    shoulder.scale.x = side
    const arm = new Group()
    arm.rotation.order = 'YXZ'
    shoulder.add(arm)
    const feathers = fan(feather, WING_FEATHERS, WING_LENGTH, WING_SHRINK, plumage.wing, vanes)
    for (const f of feathers) arm.add(f)
    // The shoulder itself: a tuft of down over where the feathers meet.
    arm.add(place(sphere(0.07, base, 18), 0.0, 0.01, -0.02, 1, 1, 0.7))
    hips.add(shoulder)
    wings.push({ arm, feathers })
  }

  // ---- head: as big as everything else put together ---------------------
  const neckY = 0.6
  const neck = new Group()
  neck.position.set(0, neckY, 0.05)
  hips.add(neck)
  const head = new Group()
  neck.add(head)

  // Wider than it is tall, like a dumpling.
  const skullCentre = new Vector3(0, 0.34, 0.03)
  const skullRadii = new Vector3(0.5, 0.42, 0.44)
  head.add(
    place(
      sphere(1, base, 44),
      skullCentre.x,
      skullCentre.y,
      skullCentre.z,
      skullRadii.x,
      skullRadii.y,
      skullRadii.z,
    ),
  )
  // The pale down of the face, as two soft cheeks either side of the beak.
  const cheekRadii = new Vector3(0.225, 0.17, 0.176)
  const cheekAt = (side: number) => new Vector3(side * 0.235, 0.185, 0.165)
  for (const side of [-1, 1]) {
    const c = cheekAt(side)
    head.add(place(sphere(1, pale, 24), c.x, c.y, c.z, cheekRadii.x, cheekRadii.y, cheekRadii.z))
  }

  // The beak: short, so the face stays a baby's, and hooked, so it stays an
  // eagle's. The lower half is hinged, for a chirp.
  const beakZ = 0.4
  const upper = new Mesh(beakGeometry(0.112, 0.25, 0.12), beakMat)
  upper.position.set(0, 0.245, beakZ)
  upper.scale.set(1, 0.84, 1)
  head.add(upper)
  // The cere: the soft yellow saddle a raptor's beak grows out of.
  head.add(place(sphere(0.082, glossy(CERE, 0.45), 18), 0, 0.283, beakZ + 0.045, 1.12, 0.6, 0.85))
  const beakTip = new Group()
  beakTip.position.set(0, 0.245 - 0.1, beakZ + 0.24)
  head.add(beakTip)
  const jaw = new Group()
  jaw.position.set(0, 0.2, beakZ - 0.01)
  const lower = new Mesh(
    beakGeometry(0.074, 0.14, 0.02),
    glossy(between(BEAK, '#b9520a', 0.35), 0.4),
  )
  lower.scale.set(1, 0.6, 1)
  lower.position.y = -0.022
  jaw.add(lower)
  // What shows when it opens.
  jaw.add(place(sphere(0.045, down(INSIDE), 14), 0, 0.0, 0.03, 1, 0.5, 1.2))
  head.add(jaw)

  // Eyes: big, glossy, wide apart and low on the face, each with a
  // highlight. Laid onto the skull so they sit on its surface whatever its
  // proportions.
  const eyes: Group[] = []
  const brows: Mesh[] = []
  const happy: Mesh[] = []
  const shut: Mesh[] = []
  const eyeMat = glossy('#ffffff', 0.12)
  eyeMat.map = eyeTexture()
  const eyeBall = eyeGeometry(0.09)
  const shine = new MeshBasicMaterial({ color: '#ffffff' })
  // A shut or happy eye is a line, and a dark line on dark plumage is no eye
  // at all: there it is drawn light instead.
  const arcMat = glossy(isLight(plumage.base) ? EYE : '#efe6da')
  const blush = glowTexture('rgba(255, 128, 140, 0.75)')
  const browMat = down(between(plumage.base, plumage.deep, 0.6))
  // An arc of a thin ring, its top at the origin, bulging upwards.
  const BROW_ARC = Math.PI * 0.62
  const browArch = new TorusGeometry(0.07, 0.011, 8, 20, BROW_ARC)
    .rotateZ(Math.PI / 2 - BROW_ARC / 2)
    .translate(0, -0.07, 0)
  for (const side of [-1, 1]) {
    const { at, normal } = onEllipsoid(skullCentre, skullRadii, side * 0.46, -0.08)
    const eye = facing(new Group(), normal)
    // Out past the fur, which would otherwise grow over it.
    eye.position.copy(at).addScaledVector(normal, FUR_LENGTH - 0.012)
    eye.add(place(new Mesh(eyeBall, eyeMat), 0, 0, 0, 1, 1.08, 0.55))
    eye.add(place(sphere(0.03, shine, 32), 0.026 * -side, 0.036, 0.046, 1.1, 0.95, 0.4))
    eye.add(place(sphere(0.013, shine, 24), -0.03 * -side, -0.032, 0.046, 1, 1, 0.4))
    head.add(eye)
    eyes.push(eye)

    // The two other ways of drawing an eye: a happy "^" and a sleeping "‿".
    const up = facing(new Mesh(new TorusGeometry(0.052, 0.014, 8, 18, Math.PI), arcMat), normal)
    up.position.copy(eye.position).addScaledVector(normal, 0.034)
    up.position.y -= 0.015
    up.visible = false
    head.add(up)
    happy.push(up)
    const lid = facing(new Mesh(new TorusGeometry(0.052, 0.013, 8, 18, Math.PI), arcMat), normal)
    lid.rotateZ(Math.PI)
    lid.position.copy(eye.position).addScaledVector(normal, 0.034)
    lid.position.y += 0.01
    lid.visible = false
    head.add(lid)
    shut.push(lid)

    // A brow over each eye: a thin, soft arch set well above it. An arch
    // reads as friendly on its own, and it is lifted at its inner end at
    // rest; brought down there, the same face looks cross.
    const over = onEllipsoid(skullCentre, skullRadii, side * 0.42, 0.34)
    // In a group that faces out of the head, so turning the brow is one
    // number whichever side it is on.
    const socket = facing(new Group(), over.normal)
    socket.position.copy(over.at).addScaledVector(over.normal, FUR_LENGTH * 0.9)
    const brow = new Mesh(browArch, browMat)
    socket.add(brow)
    head.add(socket)
    brows.push(brow)

    // Rosy cheeks, just under and outside each eye: a soft glow, not a disc.
    const cheek = onEllipsoid(cheekAt(side), cheekRadii, side * 0.5, 0.3)
    const glow = facing(
      new Mesh(
        new PlaneGeometry(0.2, 0.13),
        new MeshBasicMaterial({ map: blush, transparent: true, depthWrite: false }),
      ),
      cheek.normal,
    )
    glow.position.copy(cheek.at).addScaledVector(cheek.normal, FUR_LENGTH + 0.004)
    head.add(glow)
  }

  // The crest: the icon again, small, worn like a quiff. It does what a
  // dog's ears would -- stands when something has its attention, lies flat
  // when things have gone wrong, and wobbles after the head stops.
  const crest = new Group()
  crest.position.set(-0.05, 0.715, 0.03)
  const crestFeathers = fan(feather, 3, 0.36, 0.8, [plumage.wing[0], plumage.deep], vanes)
  for (const f of crestFeathers) crest.add(f)
  head.add(crest)

  // A drop of worry, for when it went wrong.
  const sweat = new Group()
  const tear = glossy('#9fd2ff')
  tear.transparent = true
  tear.opacity = 0.9
  sweat.add(place(sphere(0.045, tear, 16), 0, 0, 0))
  const cap = new Mesh(new ConeGeometry(0.043, 0.07, 16), tear)
  cap.position.y = 0.045
  sweat.add(cap)
  sweat.position.set(0.4, 0.52, 0.22)
  sweat.visible = false
  head.add(sweat)

  // And a bubble at the beak, asleep.
  const film = new MeshPhysicalMaterial({
    color: '#cfe9ff',
    roughness: 0.05,
    transparent: true,
    opacity: 0.4,
    clearcoat: 1,
  })
  const bubble = sphere(0.07, film, 20)
  bubble.position.set(0.07, 0.24, beakZ + 0.2)
  bubble.visible = false
  head.add(bubble)

  // ---- the laptop ------------------------------------------------------------
  //
  // Part of the bird rather than the scene, so it turns with it. Kept hidden
  // at scale zero until it types.
  const laptop = new Group()
  laptop.position.set(0, 0, 0.46)
  const shell = new MeshPhysicalMaterial({ color: '#cdc6e6', roughness: 0.4, clearcoat: 0.6 })
  laptop.add(place(new Mesh(new BoxGeometry(0.5, 0.032, 0.3), shell), 0, 0.016, 0))
  laptop.add(
    place(
      new Mesh(
        new PlaneGeometry(0.42, 0.18),
        new MeshPhysicalMaterial({ color: '#958eb2', roughness: 0.7 }),
      ),
      0,
      0.0325,
      -0.02,
    ).rotateX(-Math.PI / 2),
  )
  const lid = new Group()
  lid.position.set(0, 0.03, 0.15)
  lid.rotation.x = 0.28
  lid.add(place(new Mesh(new BoxGeometry(0.5, 0.3, 0.02), shell), 0, 0.15, 0))
  const screen = screenTexture()
  const display = new Mesh(new PlaneGeometry(0.44, 0.25), new MeshBasicMaterial({ map: screen }))
  display.position.set(0, 0.15, -0.0105)
  display.rotation.y = Math.PI
  lid.add(display)
  // A heart sticker on the back, which is the side anybody watching sees.
  const sticker = new Mesh(heartGeometry(), new MeshBasicMaterial({ color: '#ff8fb1' }))
  sticker.scale.set(0.11, 0.11, 0.02)
  sticker.position.set(0, 0.16, 0.014)
  lid.add(sticker)
  laptop.add(lid)
  laptop.scale.setScalar(0)
  root.add(laptop)

  // Fur on the big round parts -- head, cheeks, body, bib, thighs -- and
  // not on the small ones, where it would only blur them.
  const strands = strandTexture()
  const balls: [Mesh, string][] = []
  root.traverse((node) => {
    const mesh = node as Mesh
    if (!mesh.isMesh || !(mesh.geometry instanceof SphereGeometry)) return
    const size =
      mesh.geometry.parameters.radius * Math.min(mesh.scale.x, mesh.scale.y, mesh.scale.z)
    if (size < 0.1) return
    if (mesh.material === base) balls.push([mesh, plumage.base])
    else if (mesh.material === pale) balls.push([mesh, plumage.pale])
  })
  for (const [ball, hex] of balls) furOn(ball, hex, strands)

  root.traverse((node) => {
    const mesh = node as Mesh
    if (!mesh.isMesh) return
    if (mesh.userData.fur) return
    const solid = !(mesh.material as Material).transparent
    mesh.castShadow = solid
    mesh.receiveShadow = solid
  })

  return {
    root,
    hips,
    chest,
    neck,
    neckY,
    crest,
    crestFeathers,
    wings: [wings[0]!, wings[1]!],
    tail,
    tailFeathers,
    eyes: [eyes[0]!, eyes[1]!],
    brows: [brows[0]!, brows[1]!],
    happy: [happy[0]!, happy[1]!],
    shut: [shut[0]!, shut[1]!],
    beakTip,
    jaw,
    sweat,
    bubble,
    laptop,
    screen,
  }
}

/** A laptop screen with a few lines of something being written on it. */
function screenTexture(): CanvasTexture {
  const canvas = document.createElement('canvas')
  canvas.width = 128
  canvas.height = 256
  const g = canvas.getContext('2d')!
  g.fillStyle = '#2a2740'
  g.fillRect(0, 0, 128, 256)
  const colours = ['#9fe3c4', '#ffd38a', '#a9c8ff', '#f6a6c1', '#e6e1ff']
  for (let i = 0; i < 16; i++) {
    g.fillStyle = colours[i % colours.length]!
    const indent = (i * 37) % 3
    g.fillRect(10 + indent * 10, 8 + i * 16, 36 + ((i * 53) % 64), 7)
  }
  const texture = new CanvasTexture(canvas)
  texture.colorSpace = SRGBColorSpace
  texture.repeat.set(1, 0.5)
  return texture
}

// ── What floats about it ─────────────────────────────────────────────

interface Props {
  group: Group
  /** Send a few hearts up. */
  hearts(now: number): void
  /** Whether any hearts are still in the air. */
  busy(now: number): boolean
  animate(t: number, pose: Pose, rig: Rig): void
}

/** How long a heart takes to float up and fade. */
const HEART_S = 1.3

const scratch = new Vector3()

function buildProps(accent: Color): Props {
  const group = new Group()
  const ink = new MeshPhysicalMaterial({ color: accent, roughness: 0.4, clearcoat: 0.5 })

  // Three dots, rising in a bubble's arc above and to the right of the head.
  const thought = [0.035, 0.05, 0.068].map((r, i) => {
    const dot = sphere(r, ink, 16)
    dot.position.set(0.5 + i * 0.13, 1.3 + i * 0.09, 0.2)
    group.add(dot)
    return dot
  })

  // A question mark: an arc, a stem, a dot.
  const question = new Group()
  const hook = new Mesh(new TorusGeometry(0.075, 0.026, 10, 22, Math.PI * 1.35), ink)
  hook.rotation.z = -Math.PI * 0.35
  hook.position.y = 0.17
  question.add(hook)
  const stem = new Mesh(new CylinderGeometry(0.026, 0.026, 0.07, 10), ink)
  stem.position.set(0, 0.06, 0)
  question.add(stem)
  question.add(place(sphere(0.03, ink, 12), 0, -0.03, 0))
  question.position.set(0.7, 1.32, 0.15)
  group.add(question)

  // Zs, each three bars.
  const zzz = [0, 1, 2].map(() => {
    const z = new Group()
    const bar = (w: number, y: number, rot = 0) => {
      const m = new Mesh(new BoxGeometry(w, 0.024, 0.024), ink)
      m.position.y = y
      m.rotation.z = rot
      z.add(m)
    }
    bar(0.1, 0.05)
    bar(0.1, -0.05)
    bar(0.13, 0, Math.atan2(0.1, -0.1) - Math.PI)
    group.add(z)
    return z
  })

  // Little puffs of dust where it pecks.
  const puffs = [0, 1, 2].map(() => {
    const m = sphere(
      0.045,
      new MeshBasicMaterial({ color: '#b9c3cf', transparent: true, opacity: 0, depthWrite: false }),
      12,
    )
    group.add(m)
    return m
  })

  // Hearts, for a pat; each with its own material, to fade on its own.
  const heart = heartGeometry()
  const hearts = [0, 1, 2, 3].map(() => {
    const m = new Mesh(
      heart,
      new MeshPhysicalMaterial({
        color: '#ff6f9c',
        roughness: 0.35,
        clearcoat: 1,
        transparent: true,
      }),
    )
    m.visible = false
    group.add(m)
    return { mesh: m, born: -Infinity, x: 0 }
  })

  // Sparkles round the head when it is pleased with itself.
  const star = new MeshBasicMaterial({ color: '#ffcf4a' })
  const sparkles = [
    [-0.82, 1.36],
    [0.86, 1.46],
    [0.78, 0.5],
    [-0.76, 0.46],
  ].map(([x, y]) => {
    const m = new Mesh(new OctahedronGeometry(0.05), star)
    m.scale.set(0.6, 1.3, 0.3)
    m.position.set(x!, y!, 0.25)
    group.add(m)
    return m
  })

  // A soft shadow, so it sits on something rather than floating.
  const canvas = document.createElement('canvas')
  canvas.width = canvas.height = 64
  const g = canvas.getContext('2d')!
  const gradient = g.createRadialGradient(32, 32, 0, 32, 32, 32)
  gradient.addColorStop(0, 'rgba(60, 40, 30, 0.34)')
  gradient.addColorStop(1, 'rgba(60, 40, 30, 0)')
  g.fillStyle = gradient
  g.fillRect(0, 0, 64, 64)
  const shadow = new Mesh(
    new PlaneGeometry(1.2, 0.9),
    new MeshBasicMaterial({ map: new CanvasTexture(canvas), transparent: true, depthWrite: false }),
  )
  shadow.rotation.x = -Math.PI / 2
  shadow.position.set(0.02, 0.001, 0.05)
  group.add(shadow)

  return {
    group,
    hearts(now) {
      hearts.forEach((h, i) => {
        h.born = now + i * 0.16
        h.x = (i % 2 === 0 ? -1 : 1) * (0.08 + i * 0.05)
      })
    },
    busy(now) {
      return hearts.some((h) => now - h.born < HEART_S)
    },
    animate(t, pose, rig) {
      // Thought dots pulse one after another.
      thought.forEach((dot, i) => {
        const k = pose.thought * (0.75 + 0.25 * pulse(t, 1.2, 0.6, -i * 0.25))
        dot.scale.setScalar(Math.max(0.0001, k))
        dot.visible = pose.thought > 0.02
      })

      question.visible = pose.question > 0.02
      question.scale.setScalar(Math.max(0.0001, pose.question))
      question.position.y = 1.32 + Math.sin(t * 3) * 0.03
      question.rotation.z = Math.sin(t * 2) * 0.12

      // Zs drift up and out from the head, one after another, and fade.
      zzz.forEach((z, i) => {
        const age = (t * 0.45 + i / 3) % 1
        z.visible = pose.zzz > 0.02
        z.position.set(0.4 + age * 0.3, 1.0 + age * 0.46, 0.2)
        z.scale.setScalar(Math.max(0.0001, pose.zzz * (0.5 + age * 0.7) * Math.sin(age * Math.PI)))
      })

      // Dust starts at the tip of the beak, wherever it has got to.
      rig.beakTip.getWorldPosition(scratch)
      puffs.forEach((puff, i) => {
        const age = (t * 1.6 + i / 3) % 1
        const material = puff.material as MeshBasicMaterial
        puff.visible = pose.puff > 0.02
        puff.position.set(
          scratch.x + (i - 1) * 0.06 + age * 0.08,
          scratch.y - 0.02 + age * 0.12,
          scratch.z + age * 0.1,
        )
        puff.scale.setScalar(0.6 + age)
        material.opacity = pose.puff * 0.85 * Math.sin(age * Math.PI)
      })

      // Hearts float up from the head, pop in, wobble, and fade.
      for (const h of hearts) {
        const age = (t - h.born) / HEART_S
        h.mesh.visible = age >= 0 && age < 1
        if (!h.mesh.visible) continue
        const material = h.mesh.material
        h.mesh.position.set(h.x + Math.sin(age * 9) * 0.03, 1.36 + age * 0.36, 0.3)
        h.mesh.scale.setScalar(0.13 * Math.min(1, age * 6) * (1 + 0.15 * Math.sin(age * 20)))
        h.mesh.rotation.z = Math.sin(age * 7) * 0.25
        material.opacity = age > 0.65 ? 1 - (age - 0.65) / 0.35 : 1
      }

      sparkles.forEach((m, i) => {
        const twinkle = pulse(t, 0.9, 0.45, i * 0.22)
        m.visible = pose.sparkle > 0.02 && twinkle > 0.01
        m.scale.set(0.6 * twinkle, 1.3 * twinkle, 0.3).multiplyScalar(pose.sparkle)
        m.rotation.z = t * 2 + i
      })

      shadow.scale.set(1 - pose.lift * 1.5, 1, 1 - pose.lift * 1.5)
      shadow.position.x = pose.shift + 0.02
    },
  }
}

// ── Putting a pose on the bird ──────────────────────────────────────────

function lerp(a: number, b: number, k: number): number {
  return a + (b - a) * k
}

/**
 * A wing's three turns at each end of what it can do, in its own frame (+x
 * away from the body): swung down its side and wrapped back round the body,
 * folded; stood up flat to whoever is looking, raised; brought forward and
 * down onto the keys; and drawn round the front, asleep.
 */
const FOLDED = { x: 0.12, y: 0.5, z: -2.2 }
const RAISED = { x: -0.05, y: -0.12, z: -0.07 }
const TAPPING = { x: -0.95, y: -0.62, z: -2.5 }
const WRAPPED = { x: -0.5, y: -0.5, z: -2.45 }

function apply(rig: Rig, pose: Pose, t: number, blink: number) {
  rig.root.rotation.y = FACING + pose.yaw
  rig.root.position.x = pose.shift
  rig.hips.position.y = pose.lift
  rig.hips.rotation.x = pose.lean
  rig.chest.scale.y = pose.squash
  rig.neck.position.y = rig.neckY + (pose.squash - 1) * 0.45

  rig.neck.rotation.set(pose.headPitch - pose.lean * 0.6, pose.headYaw, pose.headRoll, 'YXZ')

  // The crest: up, it stands and its three feathers part; down, it lies back
  // along the head and they close.
  const up = Math.max(0, pose.crest)
  const flat = Math.max(0, -pose.crest)
  rig.crest.rotation.set(-0.16 - flat * 1.05 + up * 0.1, 0, pose.crestTilt * 0.3 - 0.06)
  rig.crestFeathers.forEach((feather, i) => {
    feather.rotation.z = -(0.06 + i * (0.5 + up * 0.22 - flat * 0.34))
  })

  // The tail: lifted when it is pleased, wagged from side to side.
  rig.tail.rotation.set(-2.05 + pose.tailLift * 0.62, pose.tailWag, 0, 'YXZ')

  rig.wings.forEach((wing, i) => {
    const raise = i === 0 ? pose.wingL : pose.wingR
    const tap = Math.max(0, Math.min(1, i === 0 ? pose.tapL : pose.tapR))
    const lifted = ease(Math.max(0, Math.min(1, raise)))
    const hang = Math.max(0, -raise)
    const wrap = ease(Math.max(0, Math.min(1, pose.wrap)))
    let x = lerp(FOLDED.x, RAISED.x, lifted)
    let y = lerp(FOLDED.y, RAISED.y, lifted)
    let z = lerp(FOLDED.z, RAISED.z, lifted)
    x = lerp(x, TAPPING.x, tap)
    y = lerp(y, TAPPING.y, tap)
    z = lerp(z, TAPPING.z, tap)
    x = lerp(x, WRAPPED.x, wrap)
    y = lerp(y, WRAPPED.y, wrap)
    z = lerp(z, WRAPPED.z, wrap)
    // Hanging: let go of, so it falls a little away from the body and forward.
    wing.arm.rotation.set(x - hang * 0.3, y - hang * 0.34, z + hang * 0.2 - pose.lean * 0.2)
    const open = lerp(WING_CLOSED, WING_OPEN, Math.max(0, Math.min(1, lifted + pose.fan * 0.42)))
    // Raised, the long feather is in front, as on the icon. Folded, the
    // short ones are, so they show as a row of tips down its side instead of
    // hiding behind the long one.
    const reach = lerp(WING_FOLDED, 1, lifted)
    const stack = 0.017 * (1 - 2 * lifted)
    wing.feathers.forEach((feather, j) => {
      const size = WING_LENGTH * Math.pow(WING_SHRINK, j) * reach
      feather.scale.set(size, size, 0.62)
      feather.position.z = j * stack
      feather.rotation.z = -(WING_LEAD + j * open)
    })
  })

  const showHappy = pose.happy > 0.5
  const showShut = !showHappy && pose.shut > 0.5
  for (let i = 0; i < 2; i++) {
    rig.eyes[i]!.visible = !showHappy && !showShut
    rig.eyes[i]!.scale.y = Math.max(0.08, pose.eyes * blink)
    rig.happy[i]!.visible = showHappy
    rig.shut[i]!.visible = showShut
  }

  // Concentrating, the inner end of each brow comes down; soft and open, it
  // goes up.
  rig.brows.forEach((brow, i) => {
    const side = i === 0 ? -1 : 1
    brow.rotation.z = side * pose.brow * 0.45
    brow.position.y = pose.brow < 0 ? pose.brow * -0.012 : 0
  })

  rig.jaw.rotation.x = Math.max(0, pose.beak) * 0.5

  rig.sweat.visible = pose.sweat > 0.05
  rig.sweat.scale.setScalar(Math.max(0.0001, pose.sweat))
  rig.sweat.position.y = 0.52 - ((t * 0.12) % 0.08)

  rig.bubble.visible = pose.bubble > 0.05
  rig.bubble.scale.setScalar(0.35 + pose.bubble * 0.85)

  rig.laptop.scale.setScalar(1.15 * ease(Math.min(1, Math.max(0, pose.laptop))))
  rig.laptop.visible = pose.laptop > 0.01
  if (pose.laptop > 0.5) rig.screen.offset.y = (t * 0.18) % 0.5
}
