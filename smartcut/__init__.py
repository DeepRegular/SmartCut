"""smartcut - frame-accurate cutting with GOP-boundary smart rendering."""
from .probe import (probe, access_points, keyframe_times, AccessPoint,
                    resolve_leading_policy, MediaInfo)
from .planner import plan, plan_range, RangePlan, Segment
from .renderer import render, RenderOptions
from .verify import verify, VerifyResult

__version__ = "0.1.0"
__all__ = ["probe", "access_points", "keyframe_times", "AccessPoint", "MediaInfo", "plan", "plan_range",
           "RangePlan", "Segment", "render", "RenderOptions",
           "verify", "VerifyResult"]
