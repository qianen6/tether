// Remodex's sidebar over the conversation: the panel slides in from the left over a dimmed
// screen. A swipe from the left edge opens it, a swipe left or a tap outside closes it.
import { useEffect, useMemo, useRef, type ReactNode } from "react";
import { Animated, Easing, Keyboard, PanResponder, useWindowDimensions, View, type PanResponderGestureState } from "react-native";
import { useTheme } from "./theme";

const EDGE = 22;
/** How far past its width the closed panel sits, so its shadow stays off screen. */
const PAST = 24;

export function Drawer(props: { open: boolean; onOpenChange: (open: boolean) => void; panel: ReactNode; children: ReactNode }) {
  const t = useTheme();
  const { width } = useWindowDimensions();
  const w = Math.min(Math.round(width * 0.86), 380);
  const x = useRef(new Animated.Value(props.open ? 0 : -w - PAST)).current;
  const from = useRef(0);
  const state = useRef({ open: props.open, w, onOpenChange: props.onOpenChange });
  state.current = { open: props.open, w, onOpenChange: props.onOpenChange };

  const settle = (open: boolean) => {
    Animated.timing(x, { toValue: open ? 0 : -state.current.w - PAST, duration: 220, easing: Easing.out(Easing.cubic), useNativeDriver: true }).start();
  };
  useEffect(() => {
    if (props.open) Keyboard.dismiss();
    settle(props.open);
  }, [props.open, w]);

  const release = (g: PanResponderGestureState) => {
    const { w: width, open, onOpenChange } = state.current;
    const at = from.current + g.dx;
    const next = g.vx > 0.3 ? true : g.vx < -0.3 ? false : at > -width / 2;
    settle(next);
    if (next !== open) onOpenChange(next);
  };
  const handlers = (tap: boolean) =>
    PanResponder.create({
      onStartShouldSetPanResponder: () => tap,
      onMoveShouldSetPanResponder: (_, g) => Math.abs(g.dx) > 10 && Math.abs(g.dx) > Math.abs(g.dy) * 1.5,
      onPanResponderGrant: () => {
        x.stopAnimation(v => (from.current = v));
        Keyboard.dismiss();
      },
      onPanResponderMove: (_, g) => x.setValue(Math.max(-state.current.w - PAST, Math.min(0, from.current + g.dx))),
      onPanResponderRelease: (_, g) => {
        // A touch that hardly moved on the dimmed screen is a tap: close.
        if (tap && Math.abs(g.dx) < 8 && Math.abs(g.dy) < 8) {
          settle(false);
          state.current.onOpenChange(false);
        } else release(g);
      },
      onPanResponderTerminate: () => settle(state.current.open),
    }).panHandlers;
  const pan = useMemo(() => handlers(false), []);
  const outside = useMemo(() => handlers(true), []);

  const dim = x.interpolate({ inputRange: [-w - PAST, 0], outputRange: [0, 0.32], extrapolate: "clamp" });
  return (
    <View style={{ flex: 1 }}>
      {props.children}
      {/* The left edge listens for a swipe while the panel is closed. */}
      {props.open ? null : <View {...pan} style={{ position: "absolute", left: 0, top: 80, bottom: 120, width: EDGE }} />}
      <Animated.View pointerEvents={props.open ? "auto" : "none"} style={{ position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: "#000", opacity: dim }}>
        <View style={{ flex: 1 }} {...outside} />
      </Animated.View>
      <Animated.View
        {...pan}
        style={{
          position: "absolute",
          left: 0,
          top: 0,
          bottom: 0,
          width: w,
          backgroundColor: t.sidebar,
          transform: [{ translateX: x }],
          shadowColor: "#000",
          shadowOpacity: 0.12,
          shadowRadius: 24,
          elevation: 16,
        }}
      >
        {props.panel}
      </Animated.View>
    </View>
  );
}
