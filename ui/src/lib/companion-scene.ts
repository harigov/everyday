// The dog itself: a small three.js scene, built and animated in code.
//
// Built rather than downloaded. The open models that exist -- Quaternius's
// CC0 Shiba Inu is the good one -- come with a walk, a gallop and an attack,
// one breed and one painted coat, and what this dog has to do is sniff, type,
// tilt its head and fall asleep, in any of five shapes and thirty-five coats.
// That is a rig nobody has published, so the dog is made of three.js's own
// spheres, capsules and lathes, every part a mesh of its own so a marking is
// a colour on a part rather than a texture to paint.
//
// What makes it cute is mostly not the shapes. It is drawn as a toy rather
// than an animal: a head bigger than the body, eyes wide apart and low, a
// small snout, and fur that is velvet -- a sheen that catches the light at
// the edges -- lit by a soft studio rather than a lamp, so it reads as
// something you could pick up. And it is alive in the small ways a toy is
// not: it blinks on its own clock, its ears lag a beat behind its head and
// wobble when it stops, it watches the pointer, and it is pleased to be
// petted.
//
// The animation is poses, not keyframes. Each act is a function from time to
// a `Pose` -- a couple of dozen numbers: how far the body leans, where the
// head points, how fast the tail goes -- and moving from one act to the next
// blends from wherever the dog was to wherever the new act puts it, so an act
// that changes mid-wag does not snap. Everything that is not an act -- the
// blinking, the ears' wobble, where it is looking, being petted -- is laid
// on top, so it carries on whatever the act.
//
// Loaded on demand (see `Companion.svelte`), so the rest of the interface
// does not carry three.js for a header that might be switched off.

import {
  BoxGeometry,
  CanvasTexture,
  CapsuleGeometry,
  CatmullRomCurve3,
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
  PerspectiveCamera,
  PlaneGeometry,
  PMREMGenerator,
  Quaternion,
  Scene,
  Shape,
  SphereGeometry,
  SRGBColorSpace,
  TorusGeometry,
  TubeGeometry,
  Vector2,
  Vector3,
  WebGLRenderer,
  type BufferGeometry,
  type Material,
  type Object3D,
} from 'three'
import { RoomEnvironment } from 'three/examples/jsm/environments/RoomEnvironment.js'
import { coatOf, type Act, type Breed, type Look } from './companion'

// ── The scene's public face ─────────────────────────────────────────────

export interface DogScene {
  setLook(look: Look): void
  setAct(act: Act): void
  /** The canvas's size in CSS pixels. */
  resize(width: number, height: number): void
  /** Run the animation. */
  play(): void
  /** Stop drawing, keeping everything for `play` to pick up. */
  pause(): void
  /** Draw the current act once, standing still: for reduced motion. */
  still(): void
  /**
   * Where the pointer is from the dog, roughly -1 to 1 each way (right and
   * down are positive). It looks there for a while, then loses interest.
   */
  lookAt(x: number, y: number): void
  /** A pat: hearts, a wiggle, happy eyes. */
  pet(): void
  dispose(): void
}

export interface DogOptions {
  look: Look
  act: Act
  /** For the thought, the question mark and the Zs: the app's accent. */
  accent: string
}

/** How long an act may idle before the dog lies down for a nap. */
const DOZE_AFTER_S = 180
/** How long a change of act takes to blend through. */
const BLEND_S = 0.45
/** Frames a second. A header ornament has no business asking for sixty. */
const FPS = 30
/**
 * Asleep, half that: it is only breathing, and a rail left open all day on
 * an assistant nobody has set up is a dog asleep all day.
 */
const SLEEP_FPS = 15
/** Which way the dog faces at rest: a touch to one side, so it reads as 3D. */
const FACING = 0.24
/** How long a pat lasts. */
const PET_S = 1.7
/** How long the pointer holds its attention after it stops moving. */
const ATTENTION_S = 3.5

/**
 * Make the scene, or `null` where WebGL is not available -- a webview with
 * hardware acceleration switched off, a context the browser refused. The
 * header then has no dog, which is the honest failure: nothing else depends
 * on it.
 */
export function createDogScene(canvas: HTMLCanvasElement, options: DogOptions): DogScene | null {
  const renderer = makeRenderer(canvas)
  if (!renderer) return null
  const scene = studio(renderer)
  const camera = new PerspectiveCamera(24, 1, 0.1, 40)

  const props = buildProps(accentOf(options.accent))
  scene.add(props.group)

  let shown = options.look
  let rig = buildDog(shown)
  scene.add(rig.root)

  let act: Act = options.act
  /** What is being acted out: `act`, unless an idle dog has nodded off. */
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

  function step(dt: number) {
    clock += dt
    const u = clock - actStart
    // Left alone long enough, an idle dog lies down. Any new act wakes it.
    const acting: Act = act === 'idle' && u > DOZE_AFTER_S ? 'sleep' : act
    if (acting !== current) {
      from = { ...pose }
      blend = 0
      current = acting
    }
    const target = poseFor(acting, clock, u)
    if (from && blend < 1) {
      blend = Math.min(1, blend + dt / BLEND_S)
      pose = mix(from, target, ease(blend))
    } else {
      pose = target
    }
    const live = life.over(pose, current, clock, dt)
    apply(rig, live, clock, blink.open(clock, live))
    life.settle(rig, live, dt)
    props.animate(clock, live, rig)
  }

  function draw() {
    renderer!.render(scene, camera)
  }

  function loop(ms: number) {
    frame = requestAnimationFrame(loop)
    if (last === 0) last = ms
    const elapsed = (ms - last) / 1000
    if (elapsed < 1 / (current === 'sleep' ? SLEEP_FPS : FPS) - 0.002) return
    last = ms
    // A tab that slept for a minute must not wake into a minute's blend.
    step(Math.min(elapsed, 0.1))
    draw()
  }

  /** Between frames, or with no loop running: bring the picture up to date. */
  function refresh() {
    if (frame) return
    step(0)
    draw()
  }

  return {
    setLook(look) {
      if (
        look.breed === shown.breed &&
        look.coat === shown.coat &&
        look.markings === shown.markings
      ) {
        return
      }
      shown = look
      scene.remove(rig.root)
      dispose(rig.root)
      rig = buildDog(look)
      scene.add(rig.root)
      refresh()
    },
    setAct(next) {
      if (next === act) return
      act = next
      actStart = clock
      blink = new Blinker(clock)
      refresh()
    },
    resize(w, h) {
      width = w
      height = h
      // Drawn at twice the size even on a 1x display: at this size the edge
      // of an ear is a handful of pixels, and supersampling is what keeps it
      // from crawling. The cost, at under 200 pixels square, is nothing.
      renderer.setPixelRatio(Math.min(3, Math.max(2, window.devicePixelRatio || 1)))
      renderer.setSize(w, h, false)
      frameCamera(camera, width, height)
      if (!frame) draw()
    },
    play() {
      if (frame) return
      last = 0
      frame = requestAnimationFrame(loop)
    },
    pause() {
      cancelAnimationFrame(frame)
      frame = 0
    },
    still() {
      cancelAnimationFrame(frame)
      frame = 0
      // Far enough into the act that it has arrived, and not blinking. A
      // pat still shows, as a happy face, for as long as it lasts.
      clock = Math.max(clock, actStart + 1.2)
      from = null
      current = act
      const live = life.over(poseFor(act, clock, 1.2), act, clock, 0, true)
      apply(rig, live, clock, 1)
      props.animate(clock, live, rig)
      draw()
    },
    lookAt(x, y) {
      life.look(x, y, clock)
    },
    pet() {
      life.pet(clock)
      props.hearts(clock)
      refresh()
    },
    dispose() {
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

/** Draws dogs to pictures, for the breed picker's cards. */
export interface Painter {
  /**
   * Each dog as a data URL, one a frame, so a row of them never holds the
   * window up for longer than one dog takes to draw.
   */
  paint(looks: Look[], width: number, height: number): Promise<string[]>
  dispose(): void
}

/**
 * A painter, or `null` without WebGL. One renderer for as long as the
 * picker is open: making one, lighting it and compiling its shaders is most
 * of the cost of a portrait, and the cards are repainted every time the
 * coat changes.
 */
export function createPainter(): Painter | null {
  const canvas = document.createElement('canvas')
  const renderer = makeRenderer(canvas)
  if (!renderer) return null
  renderer.setPixelRatio(2)
  const scene = studio(renderer)
  const camera = new PerspectiveCamera(24, 1, 0.1, 40)
  const props = buildProps(new Color('#d0782f'))
  scene.add(props.group)
  let disposed = false
  return {
    async paint(looks, width, height) {
      renderer.setSize(width, height, false)
      frameCamera(camera, width, height, 1.3)
      const out: string[] = []
      for (const look of looks) {
        if (disposed) break
        const rig = buildDog(look)
        scene.add(rig.root)
        const pose = poseFor('idle', 0, 0)
        pose.headYaw = -0.12
        pose.headRoll = 0.12
        apply(rig, pose, 0, 1)
        props.animate(0, pose, rig)
        renderer.render(scene, camera)
        out.push(canvas.toDataURL('image/png'))
        scene.remove(rig.root)
        dispose(rig.root)
        await new Promise((resolve) => requestAnimationFrame(resolve))
      }
      return out
    },
    dispose() {
      disposed = true
      dispose(scene)
      scene.environment?.dispose()
      renderer.dispose()
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
    // Neutral rather than filmic: a honey coat should come out honey, not
    // the orange a film curve would push it to.
    renderer.toneMapping = NeutralToneMapping
    renderer.setClearColor(0x000000, 0)
    return renderer
  } catch {
    return null
  }
}

/**
 * The room it is lit in: a soft studio for the fur to pick up, a warm key
 * from the front left, and a cool rim behind that the velvet catches.
 */
function studio(renderer: WebGLRenderer): Scene {
  const scene = new Scene()
  const pmrem = new PMREMGenerator(renderer)
  const room = new RoomEnvironment()
  // Small: it only has to be soft, and at this size a sharper one is a
  // quarter of a second spent on reflections nobody can see.
  scene.environment = pmrem.fromScene(room, 0.04, 0.1, 100, { size: 64 }).texture
  scene.environmentIntensity = 0.55
  room.dispose()
  pmrem.dispose()

  scene.add(new HemisphereLight(0xfff6ec, 0x9a8b80, 0.55))
  const key = new DirectionalLight(0xfff0dc, 1.7)
  key.position.set(-2.2, 3.4, 3.2)
  scene.add(key)
  const rim = new DirectionalLight(0xe4ecff, 1.6)
  rim.position.set(2.0, 2.6, -2.8)
  scene.add(rim)
  return scene
}

/**
 * Fitted to a box around the dog and what floats about it, whatever the
 * canvas's shape: the header's and the settings preview's both show the
 * whole dog. `zoom` above 1 crops in, for a portrait.
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
      const map = (m as MeshBasicMaterial).map
      if (map) map.dispose()
      m.dispose()
    }
  })
}

// ── Poses ────────────────────────────────────────────────────────────────

/** Everything an act decides about the dog, at one moment. */
interface Pose {
  /** The whole dog, turned towards the laptop (+) or the viewer. */
  yaw: number
  /** The whole dog, shuffled sideways while it sniffs around. */
  shift: number
  /** Up for a hop, down for lying flat. */
  lift: number
  /** The body's height, for breathing and landing. */
  squash: number
  /** The body, leaning forward from the hips. */
  lean: number
  headPitch: number
  headYaw: number
  headRoll: number
  /** Up and forward (+), flat and back (-). */
  earL: number
  earR: number
  tailWag: number
  /** 0 is tucked down, 1 is up and proud. */
  tailLift: number
  /** How far each front leg reaches forward, in radians. */
  pawL: number
  pawR: number
  /** 1 is open, 0 shut. Blinking is applied on top. */
  eyes: number
  /** The eyes drawn as happy arcs, past one half. */
  happy: number
  /** The eyes drawn shut, asleep, past one half. */
  shut: number
  /** The nose's twitch. */
  sniff: number
  tongue: number
  laptop: number
  thought: number
  question: number
  zzz: number
  puff: number
  /** A drop of worry on the side of the head. */
  sweat: number
  /** A bubble from the nose, asleep. */
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

/** Fades in over `edge` seconds at the start of a slot and out at its end. */
function envelope(local: number, length: number, edge: number): number {
  return ease(Math.max(0, Math.min(1, local / edge, (length - local) / edge)))
}

/** Sitting, breathing, looking a little towards whoever is looking at it. */
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
    earL: 0,
    earR: 0,
    tailWag: Math.sin(t * 3.1) * 0.18,
    tailLift: 0.75,
    pawL: 0,
    pawR: 0,
    eyes: 1,
    happy: 0,
    shut: 0,
    sniff: 0,
    tongue: 0,
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
 * something small to do -- tilt its head, look round, wag, twitch an ear,
 * bounce -- from a slot of time hashed to a number, so it is never the same
 * loop twice in a row and never needs a random generator to remember
 * anything.
 */
function poseFor(act: Act, t: number, u: number): Pose {
  const p = rest(t)
  switch (act) {
    case 'idle': {
      const LENGTH = 4.6
      const slot = Math.floor(t / LENGTH)
      const r = hash(slot)
      const side = hash(slot + 17) < 0.5 ? -1 : 1
      const w = envelope(t - slot * LENGTH, LENGTH, 0.7)
      if (r < 0.24) {
        // Sit.
      } else if (r < 0.44) {
        p.headRoll += 0.32 * side * w
        if (side > 0) p.earL += 0.3 * w
        else p.earR += 0.3 * w
      } else if (r < 0.6) {
        p.headYaw += 0.5 * side * w
        p.headPitch -= 0.06 * w
      } else if (r < 0.78) {
        p.tailWag = p.tailWag * (1 - w) + Math.sin(t * 15) * 0.55 * w
        p.earL += 0.2 * w
        p.earR += 0.2 * w
        p.tongue = 0.7 * w
        p.lift = Math.abs(Math.sin(t * 7.5)) * 0.014 * w
      } else if (r < 0.9) {
        // A happy little bounce on the spot.
        const bounce = Math.abs(Math.sin(t * 6))
        p.lift = bounce * 0.05 * w
        p.squash += (bounce - 0.5) * 0.05 * w
        p.headRoll += Math.sin(t * 6) * 0.08 * w
        p.tailWag = p.tailWag * (1 - w) + Math.sin(t * 12) * 0.4 * w
      } else {
        const twitch = pulse(t, 1.1, 0.18) * w
        if (side > 0) p.earL += 0.45 * twitch
        else p.earR += 0.45 * twitch
      }
      return p
    }
    case 'listen': {
      // Ears up, head cocked down towards where the words are being typed.
      p.headPitch = 0.16 + Math.sin(u * 1.1) * 0.03
      p.headRoll = 0.16 * Math.sin(u * 0.6)
      p.headYaw = -0.05
      p.earL += 0.42
      p.earR += 0.42
      p.tailWag = Math.sin(u * 9) * 0.32
      p.tailLift = 0.95
      return p
    }
    case 'think': {
      // Head up and to one side, ears uneven, tail slowing; thought dots.
      p.headPitch = -0.2 + Math.sin(u * 0.9) * 0.04
      p.headRoll = 0.24 + Math.sin(u * 0.7) * 0.05
      p.headYaw = 0.05 + Math.sin(u * 0.5) * 0.12
      p.earL += 0.25
      p.earR -= 0.12
      p.tailWag = Math.sin(t * 1.6) * 0.1
      p.thought = 1
      return p
    }
    case 'sniff': {
      // Turned side-on, so the nose can be seen going to the ground, and
      // sweeping along it in bursts of sniffs.
      p.lean = 0.42
      p.headPitch = 0.46 + Math.sin(u * 2.4) * 0.05
      p.headYaw = Math.sin(u * 1.5) * 0.3
      p.headRoll = Math.sin(u * 1.5) * -0.08
      p.shift = Math.sin(u * 0.75) * 0.08
      p.yaw = 0.78 + Math.sin(u * 0.75) * 0.1
      const burst = pulse(u, 1.25, 0.6)
      p.sniff = burst * (0.5 + 0.5 * Math.sin(u * 34))
      p.earL += 0.28
      p.earR += 0.28
      p.tailWag = Math.sin(u * 10) * 0.42
      p.tailLift = 0.95
      p.puff = 1
      return p
    }
    case 'type': {
      // Turned to the laptop, paws tapping in turn, a pause now and then to
      // read what it wrote -- and the tongue out, concentrating.
      const reading = pulse(u, 3.4, 0.8, 2.6)
      const typing = 1 - reading
      const k = u * 12
      p.yaw = 0.48
      p.lean = 0.12
      p.headPitch = 0.22 + Math.sin(u * 6) * 0.02 * typing - reading * 0.06
      p.headYaw = -0.05 + reading * 0.12
      p.headRoll = reading * 0.12
      p.pawL = 0.5 + 0.28 * Math.max(0, Math.sin(k)) * typing
      p.pawR = 0.5 + 0.28 * Math.max(0, Math.sin(k + Math.PI)) * typing
      p.earL -= 0.08
      p.earR -= 0.08
      p.tailWag = Math.sin(u * 2.2) * 0.12
      p.tongue = 0.45 * typing
      p.laptop = 1
      return p
    }
    case 'ask': {
      // Looking straight at you, head cocked, a paw up -- toe beans out:
      // well?
      p.headYaw = -0.2
      p.headRoll = -0.32 + Math.sin(u * 1.4) * 0.04
      p.headPitch = -0.06
      p.earL += 0.35
      p.earR += 0.35
      p.pawR = 1.15 + Math.sin(u * 5) * 0.14
      p.tailWag = Math.sin(u * 13) * 0.45
      p.question = 1
      return p
    }
    case 'cheer': {
      const hop = Math.abs(Math.sin(u * Math.PI * 2.3))
      p.lift = hop * 0.12
      p.squash = 0.94 + hop * 0.1
      p.headYaw = -0.15
      p.headPitch = -0.1
      p.earL += 0.25 + hop * 0.2
      p.earR += 0.25 + hop * 0.2
      p.pawL = 0.4 * hop
      p.pawR = 0.4 * hop
      p.tailWag = Math.sin(u * 19) * 0.62
      p.tailLift = 1
      p.happy = 1
      p.tongue = 1
      p.sparkle = 1
      return p
    }
    case 'droop': {
      p.lean = 0.06
      p.squash = 0.97 + Math.sin(t * 1.3) * 0.01
      p.headPitch = 0.34
      p.headYaw = -0.12
      p.earL -= 0.7
      p.earR -= 0.7
      p.tailWag = 0
      p.tailLift = 0.08
      p.eyes = 0.55
      p.sweat = 1
      return p
    }
    case 'sleep': {
      // Lying down: lower, leaning onto front legs stretched out, head on
      // them, breathing slowly; a bubble at the nose, and the Zs.
      const breath = Math.sin(t * 1.25)
      p.lift = -0.08
      p.lean = 0.36
      p.squash = 0.9 + breath * 0.03
      p.headPitch = 0.3 + breath * 0.02
      p.headYaw = -0.18
      p.headRoll = 0.26
      p.pawL = 1.05
      p.pawR = 1.05
      p.earL -= 0.3
      p.earR -= 0.3
      p.tailWag = 0
      p.tailLift = 0.15
      p.eyes = 0
      p.shut = 1
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
 * after the pose is on the dog, the ears catching up with the head.
 */
class Life {
  #look = new Vector2()
  #gaze = new Vector2()
  #lookedAt = -Infinity
  #petAt = -Infinity
  #ears = [new Spring(), new Spring()]
  #head = new Vector3()
  #lift = 0

  look(x: number, y: number, now: number) {
    this.#look.set(Math.max(-1, Math.min(1, x)), Math.max(-1, Math.min(1, y)))
    this.#lookedAt = now
  }

  pet(now: number) {
    this.#petAt = now
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
      p.earL -= 0.25 * w
      p.earR -= 0.25 * w
      p.tailWag = p.tailWag * (1 - w) + Math.sin(since * 20) * 0.6 * w
      p.squash += Math.abs(wiggle) * 0.04 * w
      p.tongue = Math.max(p.tongue, 0.8 * w)
      p.zzz *= 1 - w
      p.bubble *= 1 - w
    }
    return p
  }

  /**
   * The ears, a beat behind the head: each one a spring pushed by how fast
   * the head turned and how hard the body landed, so a nod makes them flap
   * and a hop makes them bounce.
   */
  settle(rig: Rig, pose: Pose, dt: number) {
    if (dt <= 0) return
    const head = new Vector3(pose.headPitch, pose.headYaw, pose.headRoll)
    const turn = head.clone().sub(this.#head).divideScalar(dt)
    const lift = (pose.lift - this.#lift) / dt
    this.#head.copy(head)
    this.#lift = pose.lift
    rig.ears.forEach((ear, i) => {
      const side = i === 0 ? -1 : 1
      const spring = this.#ears[i]!
      spring.step(dt, turn.x * 0.06 - lift * 0.5 + side * (turn.z + turn.y) * 0.04)
      ear.rotation.x += spring.x * (rig.earKind === 'pointy' ? 0.6 : 1)
      ear.rotation.z += side * spring.x * (rig.earKind === 'floppy' ? 1.2 : 0.5)
    })
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

// ── The dog ──────────────────────────────────────────────────────────────

/** What makes a corgi a corgi, as numbers. */
interface Anatomy {
  ears: 'pointy' | 'floppy' | 'button' | 'fluffy'
  earSize: number
  /** Muzzle length; a pug's is half a shiba's. */
  snout: number
  tail: 'curl' | 'stub' | 'plume' | 'pompom'
  /** Body width. */
  chubby: number
  /** Leg length: a corgi sits lower. */
  legs: number
  cheeks: boolean
  topknot: boolean
}

const SHAPES: Record<Breed, Anatomy> = {
  shiba: {
    ears: 'pointy',
    earSize: 1,
    snout: 1,
    tail: 'curl',
    chubby: 1,
    legs: 1,
    cheeks: true,
    topknot: false,
  },
  corgi: {
    ears: 'pointy',
    earSize: 1.4,
    snout: 1.05,
    tail: 'stub',
    chubby: 1.1,
    legs: 0.75,
    cheeks: true,
    topknot: false,
  },
  beagle: {
    ears: 'floppy',
    earSize: 1,
    snout: 1.2,
    tail: 'plume',
    chubby: 1,
    legs: 1,
    cheeks: false,
    topknot: false,
  },
  pug: {
    ears: 'button',
    earSize: 1,
    snout: 0.55,
    tail: 'curl',
    chubby: 1.16,
    legs: 0.9,
    cheeks: true,
    topknot: false,
  },
  poodle: {
    ears: 'fluffy',
    earSize: 1,
    snout: 1.1,
    tail: 'pompom',
    chubby: 0.96,
    legs: 1.05,
    cheeks: false,
    topknot: true,
  },
}

/** The parts that move, and where they started. */
interface Rig {
  root: Group
  hips: Group
  chest: Group
  neck: Group
  neckY: number
  ears: [Group, Group]
  earBase: [number, number]
  earKind: Anatomy['ears']
  tail: Group
  legs: [Group, Group]
  eyes: [Group, Group]
  happy: [Mesh, Mesh]
  shut: [Mesh, Mesh]
  nose: Mesh
  tongue: Mesh
  sweat: Group
  bubble: Mesh
  laptop: Group
  screen: CanvasTexture
}

const WHITE = '#fbf6ee'
const NOSE = '#2b2325'
const EYE = '#191415'
const PINK = '#f4aeb0'
const BEANS = '#f39aa3'
const TONGUE = '#f27a86'

/**
 * Velvet: the fur's sheen is what makes it read as a soft toy rather than a
 * painted figure -- a pale glow at the edges, where the light grazes it.
 */
function fur(hex: string): MeshPhysicalMaterial {
  return new MeshPhysicalMaterial({
    color: hex,
    roughness: 0.78,
    metalness: 0,
    sheen: 0.8,
    sheenRoughness: 0.45,
    sheenColor: new Color(hex).lerp(new Color('#ffffff'), 0.4),
  })
}

/** Wet and glossy, with a clear coat: eyes and noses. */
function glossy(hex: string): MeshPhysicalMaterial {
  return new MeshPhysicalMaterial({
    color: hex,
    roughness: 0.25,
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

/**
 * A colour's brightness scaled by `k`, as it looks rather than as the light
 * adds up: `Color` holds linear values, where halving the number is nowhere
 * near half as dark, so the factor is taken through the display's gamma.
 */
function shade(hex: string, k: number): string {
  return `#${new Color(hex).multiplyScalar(Math.pow(k, 2.2)).getHexString()}`
}

/** Is this coat light enough that its mask should be much darker than it? */
function isLight(hex: string): boolean {
  const n = parseInt(hex.slice(1), 16)
  const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255]
  return (r * 0.3 + g * 0.59 + b * 0.11) / 255 > 0.5
}

/**
 * A point on an ellipsoid's surface, and which way is out, for laying a spot
 * or an eye onto a body that is not a sphere.
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

/** A flat oval patch lying on a surface: a spot, a patch, a brow. */
function patch(
  material: Material,
  at: Vector3,
  normal: Vector3,
  size: number,
  stretch = 1,
  depth = 0.32,
): Mesh {
  const mesh = facing(sphere(size, material, 18), normal)
  mesh.scale.set(stretch, 1, depth)
  mesh.position.copy(at).addScaledVector(normal, -size * 0.12)
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
 * A cone with a rounded tip and a soft base, for an ear: a plain cone's
 * point is sharp, and nothing sharp is cute.
 */
function earGeometry(radius: number, height: number): LatheGeometry {
  const points: Vector2[] = []
  const STEPS = 14
  for (let i = 0; i <= STEPS; i++) {
    const k = i / STEPS
    // Fat at the base, tapering, and rounding off over the last stretch.
    const r =
      radius *
      Math.pow(1 - k, 0.8) *
      (k > 0.82 ? Math.sqrt(Math.max(0, 1 - ((k - 0.82) / 0.18) ** 2)) : 1)
    points.push(new Vector2(Math.max(0.0001, r), height * k))
  }
  return new LatheGeometry(points, 24)
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

function buildDog(look: Look): Rig {
  const shape = SHAPES[look.breed]
  const coat = coatOf(look.coat)
  const marks = look.markings
  const base = fur(coat.base)
  const white = fur(WHITE)
  const mark = fur(coat.mark)
  // A mask is always darker than the coat it sits on -- on a charcoal dog,
  // the cream that `mark` would give is a mask drawn the wrong way round.
  const maskHex = isLight(coat.base) ? shade(coat.base, 0.48) : shade(coat.base, 0.62)
  const maskTone = fur(maskHex)
  const pale = marks === 'socks' || marks === 'patches' ? white : base
  const muzzleMat = marks === 'mask' ? maskTone : pale
  const earMat = marks === 'mask' ? maskTone : base
  const innerEar = fur(PINK)
  const beans = fur(BEANS)

  const root = new Group()
  root.rotation.y = FACING
  const hips = new Group()
  root.add(hips)

  // How much lower a short-legged dog sits: everything above the ground
  // comes down by it, and the front legs are that much shorter.
  const drop = (1 - shape.legs) * 0.22
  const w = shape.chubby

  // ---- body: a small bean, mostly hidden behind a big head ------------
  const chest = new Group()
  hips.add(chest)
  const torsoCentre = new Vector3(0, 0.32 - drop * 0.6, -0.03)
  const torsoRadii = new Vector3(0.33 * w, 0.33 - drop * 0.2, 0.3)
  chest.add(
    place(
      sphere(1, base, 36),
      torsoCentre.x,
      torsoCentre.y,
      torsoCentre.z,
      torsoRadii.x,
      torsoRadii.y,
      torsoRadii.z,
    ),
  )
  // The bib: white on a dog with socks or patches, the coat otherwise --
  // and there either way, so the chest has the same soft swell.
  chest.add(place(sphere(1, pale, 28), 0, torsoCentre.y + 0.03, 0.12, 0.22 * w, 0.25, 0.18))

  if (marks === 'spots') {
    // Fixed places rather than random ones, so the same dog has the same
    // spots every time it is drawn.
    const SPOTS: [number, number, number][] = [
      [0.9, 0.35, 0.065],
      [-1.1, 0.1, 0.06],
      [2.3, 0.4, 0.07],
      [-2.4, 0.15, 0.065],
      [3.0, -0.2, 0.055],
      [1.6, -0.35, 0.05],
      [-1.7, -0.4, 0.055],
    ]
    for (const [theta, phi, size] of SPOTS) {
      const { at, normal } = onEllipsoid(torsoCentre, torsoRadii, theta, phi)
      chest.add(patch(mark, at, normal, size))
    }
  }
  if (marks === 'patches') {
    // A saddle over the back.
    chest.add(
      place(
        sphere(1, mark, 32),
        0,
        torsoCentre.y + 0.06,
        torsoCentre.z - 0.05,
        torsoRadii.x * 1.03,
        torsoRadii.y * 0.78,
        torsoRadii.z * 1.02,
      ),
    )
  }

  // Hind legs: round haunches, and little feet poking out in front.
  for (const side of [-1, 1]) {
    hips.add(place(sphere(0.16, base), side * 0.2 * w, 0.15 - drop * 0.3, -0.04, 0.8, 0.95, 1.15))
    const foot = marks === 'socks' || marks === 'patches' ? white : base
    hips.add(place(sphere(0.08, foot), side * 0.22 * w, 0.045, 0.14, 1, 0.65, 1.4))
  }

  // Front legs, short and round, from the shoulder to a mitten of a paw --
  // with toe beans underneath, for when a paw is raised.
  const shoulderY = 0.4 - drop
  const legLength = shoulderY - 0.06
  const legs: Group[] = []
  for (const side of [-1, 1]) {
    const leg = new Group()
    leg.position.set(side * 0.13 * w, shoulderY, 0.14)
    const limb = new Mesh(new CapsuleGeometry(0.075, Math.max(0.02, legLength - 0.1), 8, 16), base)
    limb.position.y = -legLength / 2 + 0.02
    leg.add(limb)
    const paw = marks === 'socks' || marks === 'patches' ? white : base
    leg.add(place(sphere(0.088, paw), 0, -legLength, 0.03, 1, 0.78, 1.2))
    const down = new Vector3(0, -1, 0.25).normalize()
    const pad = patch(beans, new Vector3(0, -legLength - 0.062, 0.025), down, 0.04, 1.15, 0.35)
    leg.add(pad)
    for (const [x, z] of [
      [-0.04, 0.075],
      [0, 0.092],
      [0.04, 0.075],
    ] as const) {
      leg.add(patch(beans, new Vector3(x, -legLength - 0.052, z), down, 0.018, 1, 0.45))
    }
    if (shape.topknot) leg.add(place(sphere(0.1, base, 18), 0, -legLength + 0.1, 0.01, 1, 0.8, 1))
    hips.add(leg)
    legs.push(leg)
  }

  // ---- tail ----------------------------------------------------------------
  const tail = new Group()
  tail.position.set(0, 0.2 - drop * 0.5, -0.3)
  hips.add(tail)
  const tip = marks === 'socks' || marks === 'patches' ? white : base
  switch (shape.tail) {
    case 'curl': {
      // A doughnut of fur curled over the back.
      const curl = new Mesh(new TorusGeometry(0.1, 0.058, 14, 28, Math.PI * 1.55), base)
      curl.rotation.set(0, Math.PI / 2, -0.4)
      curl.position.set(0, 0.11, -0.05)
      tail.add(curl)
      tail.add(place(sphere(0.064, tip, 18), 0, 0.2, 0.02))
      break
    }
    case 'stub':
      tail.add(place(sphere(0.09, base, 18), 0, 0.02, -0.02, 1, 0.9, 0.8))
      break
    case 'plume': {
      const stalk = new Mesh(new CapsuleGeometry(0.048, 0.24, 8, 14), base)
      stalk.position.set(0, 0.15, -0.07)
      stalk.rotation.x = -0.45
      tail.add(stalk)
      tail.add(place(sphere(0.058, tip, 18), 0, 0.3, -0.14))
      break
    }
    case 'pompom': {
      const stalk = new Mesh(new CapsuleGeometry(0.028, 0.16, 6, 10), base)
      stalk.position.set(0, 0.1, -0.05)
      stalk.rotation.x = -0.45
      tail.add(stalk)
      tail.add(place(sphere(0.11, base, 20), 0, 0.23, -0.12))
      break
    }
  }

  // ---- head: as big as everything else put together ---------------------
  const neckY = 0.6 - drop
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
  const cheekRadii = new Vector3(0.216, 0.164, 0.176)
  const cheekAt = (side: number) => new Vector3(side * 0.25, 0.18, 0.16)
  if (shape.cheeks) {
    for (const side of [-1, 1]) {
      const c = cheekAt(side)
      head.add(place(sphere(1, pale, 24), c.x, c.y, c.z, cheekRadii.x, cheekRadii.y, cheekRadii.z))
    }
  }
  if (shape.topknot) {
    for (const [x, y, z, r] of [
      [0, 0.76, 0.0, 0.17],
      [-0.14, 0.71, -0.06, 0.13],
      [0.14, 0.71, -0.06, 0.13],
      [0, 0.7, 0.14, 0.13],
    ] as const) {
      head.add(place(sphere(r, base, 20), x, y, z))
    }
  }

  // A mask runs from the muzzle up round both eyes as one dark shape --
  // flatter than a spot, so the eyes still sit on top of it.
  if (marks === 'mask') {
    const face = onEllipsoid(skullCentre, skullRadii, 0, -0.1)
    head.add(patch(maskTone, face.at, face.normal, 0.19, 1.6, 0.17))
  }

  // The muzzle: small, so the face stays a baby's.
  const snout = shape.snout
  const muzzleZ = 0.36 + 0.04 * snout
  head.add(place(sphere(1, muzzleMat, 28), 0, 0.18, muzzleZ, 0.17, 0.12, 0.13 * snout))
  const noseZ = muzzleZ + 0.13 * snout - 0.004
  const nose = place(sphere(0.047, glossy(NOSE), 18), 0, 0.225, noseZ, 1.4, 0.88, 0.9)
  head.add(nose)

  // A small "ω" of a mouth under the nose.
  const mouth = new Mesh(
    new TubeGeometry(
      new CatmullRomCurve3([
        new Vector3(-0.04, 0.006, -0.008),
        new Vector3(-0.031, -0.008, -0.002),
        new Vector3(-0.018, -0.013, 0.001),
        new Vector3(-0.006, -0.007, 0.003),
        new Vector3(0, 0, 0.004),
        new Vector3(0.006, -0.007, 0.003),
        new Vector3(0.018, -0.013, 0.001),
        new Vector3(0.031, -0.008, -0.002),
        new Vector3(0.04, 0.006, -0.008),
      ]),
      32,
      0.0075,
      6,
    ),
    glossy('#3a2a2b'),
  )
  // On the muzzle's surface at that height, wherever a longer snout puts it.
  const mouthY = 0.138
  const mouthZ = muzzleZ + 0.13 * snout * Math.sqrt(1 - ((0.18 - mouthY) / 0.12) ** 2)
  mouth.position.set(0, mouthY, mouthZ - 0.004)
  mouth.rotation.x = -0.35
  head.add(mouth)
  const tongue = place(sphere(0.05, fur(TONGUE), 16), 0, 0.1, noseZ - 0.026, 1, 0.55, 0.6)
  head.add(tongue)

  // Eyes: big, glossy, wide apart and low on the face, each with a
  // highlight. Laid onto the skull so they sit on its surface whatever its
  // proportions.
  const eyes: Group[] = []
  const happy: Mesh[] = []
  const shut: Mesh[] = []
  const eyeMat = glossy(EYE)
  const shine = new MeshBasicMaterial({ color: '#ffffff' })
  // A shut or happy eye is a line, and a dark line on a dark face is no eye
  // at all: on a black dog, or inside a mask, it is drawn light instead.
  const arcMat = glossy(isLight(marks === 'mask' ? maskHex : coat.base) ? EYE : '#efe6da')
  const blush = glowTexture('rgba(255, 128, 140, 0.75)')
  for (const side of [-1, 1]) {
    const { at, normal } = onEllipsoid(skullCentre, skullRadii, side * 0.44, -0.05)
    if (marks === 'mask') head.add(patch(maskTone, at, normal, 0.13, 1.1, 0.2))
    if (marks === 'patches' && side > 0) {
      // Up and out towards the ear, the way a real one runs, rather than a
      // ring around the eye -- which reads as a monocle.
      const over = onEllipsoid(skullCentre, skullRadii, 0.78, 0.26)
      head.add(patch(mark, over.at, over.normal, 0.15, 1.15))
    }
    const eye = facing(new Group(), normal)
    eye.position.copy(at).addScaledVector(normal, -0.014)
    eye.add(place(sphere(0.074, eyeMat, 24), 0, 0, 0, 1, 1.14, 0.55))
    eye.add(place(sphere(0.026, shine, 12), 0.022 * -side, 0.032, 0.038))
    eye.add(place(sphere(0.012, shine, 10), -0.022 * -side, -0.028, 0.038))
    head.add(eye)
    eyes.push(eye)

    // The two other ways of drawing an eye: a happy "^" and a sleeping "‿".
    const up = facing(new Mesh(new TorusGeometry(0.052, 0.014, 8, 18, Math.PI), arcMat), normal)
    up.position.copy(eye.position).addScaledVector(normal, 0.034)
    up.position.y -= 0.015
    up.visible = false
    head.add(up)
    happy.push(up)
    const down = facing(new Mesh(new TorusGeometry(0.052, 0.013, 8, 18, Math.PI), arcMat), normal)
    down.rotateZ(Math.PI)
    down.position.copy(eye.position).addScaledVector(normal, 0.034)
    down.position.y += 0.01
    down.visible = false
    head.add(down)
    shut.push(down)

    // Rosy cheeks, just under and outside each eye: a soft glow, not a
    // disc -- on the fluff of the cheek where there is some.
    const cheek = shape.cheeks
      ? onEllipsoid(cheekAt(side), cheekRadii, side * 0.5, 0.3)
      : onEllipsoid(skullCentre, skullRadii, side * 0.62, -0.2)
    const glow = facing(
      new Mesh(
        new PlaneGeometry(0.2, 0.13),
        new MeshBasicMaterial({ map: blush, transparent: true, depthWrite: false }),
      ),
      cheek.normal,
    )
    glow.position.copy(cheek.at).addScaledVector(cheek.normal, 0.008)
    head.add(glow)

    // A shiba's "maro" eyebrows, on a dog with a white bib.
    if (marks === 'socks') {
      const brow = onEllipsoid(skullCentre, skullRadii, side * 0.34, 0.34)
      head.add(patch(white, brow.at, brow.normal, 0.036, 1.3))
    }
  }

  // Ears.
  const ears: Group[] = []
  const earBase: number[] = []
  const s = shape.earSize
  for (const side of [-1, 1]) {
    const ear = new Group()
    const thisEar = marks === 'patches' && side < 0 ? mark : earMat
    // A shade darker than the head on a floppy-eared dog, or a solid one's
    // ears vanish into it and it looks like it has a haircut.
    const flop = thisEar === base ? fur(shade(coat.base, 0.84)) : thisEar
    switch (shape.ears) {
      case 'pointy': {
        ear.position.set(side * 0.25, 0.64, -0.02)
        const outer = new Mesh(earGeometry(0.13 * s, 0.26 * s), thisEar)
        outer.scale.z = 0.45
        ear.add(outer)
        const inner = new Mesh(earGeometry(0.08 * s, 0.18 * s), innerEar)
        inner.scale.z = 0.25
        inner.position.set(0, 0.02, 0.035)
        ear.add(inner)
        ear.rotation.set(-0.12, 0, -side * 0.36)
        earBase.push(-side * 0.36)
        break
      }
      case 'floppy': {
        ear.position.set(side * 0.43, 0.52, -0.02)
        ear.add(place(sphere(0.14, flop, 24), 0, -0.17, 0, 0.5, 1.45, 0.95))
        ear.rotation.set(0, 0, side * 0.22)
        earBase.push(side * 0.22)
        break
      }
      case 'button': {
        ear.position.set(side * 0.33, 0.62, 0.04)
        const fold = place(sphere(0.13, flop, 18), 0, -0.02, 0.03, 1.1, 0.72, 0.42)
        fold.rotation.x = 0.7
        ear.add(fold)
        ear.rotation.set(0, 0, -side * 0.38)
        earBase.push(-side * 0.38)
        break
      }
      case 'fluffy': {
        ear.position.set(side * 0.43, 0.5, -0.02)
        for (const [y, r] of [
          [-0.06, 0.1],
          [-0.18, 0.11],
          [-0.3, 0.1],
        ] as const) {
          ear.add(place(sphere(r, thisEar, 18), side * 0.02, y, 0))
        }
        ear.rotation.set(0, 0, side * 0.14)
        earBase.push(side * 0.14)
        break
      }
    }
    head.add(ear)
    ears.push(ear)
  }

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

  // And a bubble from the nose, asleep.
  const film = new MeshPhysicalMaterial({
    color: '#cfe9ff',
    roughness: 0.05,
    transparent: true,
    opacity: 0.4,
    clearcoat: 1,
  })
  const bubble = sphere(0.07, film, 20)
  bubble.position.set(0.05, 0.19, noseZ + 0.05)
  bubble.visible = false
  head.add(bubble)

  // ---- the laptop ------------------------------------------------------------
  //
  // Part of the dog rather than the scene, so it turns with it. Kept hidden
  // at scale zero until it types.
  const laptop = new Group()
  laptop.position.set(0, 0, 0.44)
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

  return {
    root,
    hips,
    chest,
    neck,
    neckY,
    ears: [ears[0]!, ears[1]!],
    earBase: [earBase[0]!, earBase[1]!],
    earKind: shape.ears,
    tail,
    legs: [legs[0]!, legs[1]!],
    eyes: [eyes[0]!, eyes[1]!],
    happy: [happy[0]!, happy[1]!],
    shut: [shut[0]!, shut[1]!],
    nose,
    tongue,
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
  animate(t: number, pose: Pose, rig: Rig): void
}

const scratch = new Vector3()

function buildProps(accent: Color): Props {
  const group = new Group()
  const ink = new MeshPhysicalMaterial({ color: accent, roughness: 0.4, clearcoat: 0.5 })

  // Three dots, rising in a bubble's arc above and to the right of the head.
  const thought = [0.035, 0.05, 0.068].map((r, i) => {
    const dot = sphere(r, ink, 16)
    dot.position.set(0.46 + i * 0.13, 1.28 + i * 0.09, 0.2)
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
  question.position.set(0.66, 1.28, 0.15)
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

  // Little puffs of breath when it sniffs.
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
    [-0.55, 1.2],
    [0.6, 1.36],
    [0.5, 0.78],
    [-0.5, 0.72],
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
    animate(t, pose, rig) {
      // Thought dots pulse one after another.
      thought.forEach((dot, i) => {
        const k = pose.thought * (0.75 + 0.25 * pulse(t, 1.2, 0.6, -i * 0.25))
        dot.scale.setScalar(Math.max(0.0001, k))
        dot.visible = pose.thought > 0.02
      })

      question.visible = pose.question > 0.02
      question.scale.setScalar(Math.max(0.0001, pose.question))
      question.position.y = 1.28 + Math.sin(t * 3) * 0.03
      question.rotation.z = Math.sin(t * 2) * 0.12

      // Zs drift up and out from the head, one after another, and fade.
      zzz.forEach((z, i) => {
        const age = (t * 0.45 + i / 3) % 1
        z.visible = pose.zzz > 0.02
        z.position.set(0.4 + age * 0.3, 1.0 + age * 0.46, 0.2)
        z.scale.setScalar(Math.max(0.0001, pose.zzz * (0.5 + age * 0.7) * Math.sin(age * Math.PI)))
      })

      // Puffs start at the nose wherever it has got to.
      rig.nose.getWorldPosition(scratch)
      puffs.forEach((puff, i) => {
        const age = (t * 1.6 + i / 3) % 1
        const material = puff.material as MeshBasicMaterial
        puff.visible = pose.puff > 0.02
        puff.position.set(
          scratch.x + (i - 1) * 0.06 + age * 0.08,
          scratch.y - 0.04 + age * 0.12,
          scratch.z + age * 0.1,
        )
        puff.scale.setScalar(0.6 + age)
        material.opacity = pose.puff * 0.85 * Math.sin(age * Math.PI)
      })

      // Hearts float up from the head, pop in, wobble, and fade.
      const HEART_S = 1.3
      for (const h of hearts) {
        const age = (t - h.born) / HEART_S
        h.mesh.visible = age >= 0 && age < 1
        if (!h.mesh.visible) continue
        const material = h.mesh.material
        h.mesh.position.set(h.x + Math.sin(age * 9) * 0.03, 1.32 + age * 0.4, 0.3)
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

// ── Putting a pose on the dog ───────────────────────────────────────────

function apply(rig: Rig, pose: Pose, t: number, blink: number) {
  rig.root.rotation.y = FACING + pose.yaw
  rig.root.position.x = pose.shift
  rig.hips.position.y = pose.lift
  rig.hips.rotation.x = pose.lean
  rig.chest.scale.y = pose.squash
  rig.neck.position.y = rig.neckY + (pose.squash - 1) * 0.45

  rig.neck.rotation.set(pose.headPitch - pose.lean * 0.6, pose.headYaw, pose.headRoll, 'YXZ')

  // Ears: a perk tips a pointed ear forward and up, a droop lays it back;
  // a floppy ear swings out instead, which is how a beagle perks.
  const pointed = rig.earKind === 'pointy' || rig.earKind === 'button'
  rig.ears.forEach((ear, i) => {
    const side = i === 0 ? -1 : 1
    const amount = i === 0 ? pose.earL : pose.earR
    if (pointed) {
      // Rotating about z by -side tips an ear outwards.
      ear.rotation.x = -0.12 + amount * (amount < 0 ? 0.85 : 0.35)
      ear.rotation.z = rig.earBase[i]! - side * Math.max(0, -amount) * 0.4
    } else {
      ear.rotation.x = amount * -0.25
      ear.rotation.z = rig.earBase[i]! + side * amount * 0.35
    }
  })

  rig.tail.rotation.set(-0.5 + pose.tailLift * 0.5, 0, pose.tailWag)

  // The front legs are children of the hips, so a lean would swing them
  // back under the body; taking the lean off keeps the paws on the ground.
  rig.legs[0].rotation.x = -(pose.pawL + pose.lean)
  rig.legs[1].rotation.x = -(pose.pawR + pose.lean)

  const showHappy = pose.happy > 0.5
  const showShut = !showHappy && pose.shut > 0.5
  for (let i = 0; i < 2; i++) {
    rig.eyes[i]!.visible = !showHappy && !showShut
    rig.eyes[i]!.scale.y = Math.max(0.08, pose.eyes * blink)
    rig.happy[i]!.visible = showHappy
    rig.shut[i]!.visible = showShut
  }

  rig.nose.scale.set(1.4 * (1 + pose.sniff * 0.18), 0.88 * (1 + pose.sniff * 0.1), 0.9)
  rig.tongue.scale.set(pose.tongue, 0.55 * pose.tongue, 0.6 * pose.tongue)
  rig.tongue.visible = pose.tongue > 0.03
  rig.tongue.position.y = 0.1 - pose.tongue * 0.035

  rig.sweat.visible = pose.sweat > 0.05
  rig.sweat.scale.setScalar(Math.max(0.0001, pose.sweat))
  rig.sweat.position.y = 0.52 - ((t * 0.12) % 0.08)

  rig.bubble.visible = pose.bubble > 0.05
  rig.bubble.scale.setScalar(0.35 + pose.bubble * 0.85)

  rig.laptop.scale.setScalar(1.15 * ease(Math.min(1, Math.max(0, pose.laptop))))
  rig.laptop.visible = pose.laptop > 0.01
  if (pose.laptop > 0.5) rig.screen.offset.y = (t * 0.18) % 0.5
}
