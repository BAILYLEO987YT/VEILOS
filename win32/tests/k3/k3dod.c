// K3: a WDDM display-only driver (KMDOD) for VEILOS's dxgkrnl, stage W3
// (WDDM_DESIGN.md). Written the way Microsoft's KMDOD sample works: it
// registers with DxgkInitializeDisplayOnlyDriver (displib.lib), takes the
// POST display (the framebuffer the firmware, or VEILOS, set up) with
// DxgkCbAcquirePostDisplayOwnership, reports one video output with an EDID
// of its own, fills VidPN source and target mode sets with that mode,
// commits it, and copies what VEILOS's desktop presents
// (DxgkDdiPresentDisplayOnly) to the framebuffer. When it stops it hands
// the display back.
//
// Built with MSVC against the WDK (build.sh). Matches any VGA-compatible
// adapter (k3dod.inf: PCI\CC_0300), as KMDOD does.
#include <ntddk.h>
#include <dispmprt.h>

typedef struct {
    PDEVICE_OBJECT Pdo;
    DXGKRNL_INTERFACE Dxgk;
    DXGK_DEVICE_INFO DeviceInfo;
    DXGK_DISPLAY_INFORMATION Post;
    PUCHAR Fb;
    USHORT Vendor, Device;
    BOOLEAN Visible, Committed;
    ULONG Presents, Rects;
    ULONGLONG Pixels;
    UCHAR Edid[128];
} K3;

#define LOG(fmt, ...) DbgPrint("k3dod: " fmt "\n", __VA_ARGS__)

static ULONG Bpp(D3DDDIFORMAT f) {
    return (f == D3DDDIFMT_A8R8G8B8 || f == D3DDDIFMT_X8R8G8B8) ? 4 : 0;
}

// -----| EDID: 128 bytes describing a monitor with the POST mode |-----

static void MakeEdid(K3 *k) {
    UCHAR *e = k->Edid;
    ULONG w = k->Post.Width, h = k->Post.Height, hb = w / 4, vb = h / 20 + 3;
    ULONG clock = (w + hb) * (h + vb) * 60 / 10000; // 10 kHz units
    ULONG i, sum = 0;
    static const UCHAR header[8] = { 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0 };
    static const char name[] = "VEILOS K3\n   ";
    RtlZeroMemory(e, 128);
    RtlCopyMemory(e, header, 8);
    e[8] = 0x58; e[9] = 0xA9;             // manufacturer "VEI"
    e[10] = 0x03; e[11] = 0x00;           // product
    e[16] = 1; e[17] = 36;                // week 1, 2026
    e[18] = 1; e[19] = 4;                 // EDID 1.4
    e[20] = 0xA5;                         // digital, 8 bits per colour, DisplayPort
    e[24] = 0x06;                         // sRGB, preferred timing is native
    // Detailed timing 1: the POST mode at 60 Hz.
    e[54] = (UCHAR)clock; e[55] = (UCHAR)(clock >> 8);
    e[56] = (UCHAR)w; e[57] = (UCHAR)hb; e[58] = (UCHAR)(((w >> 8) & 0xF) << 4 | ((hb >> 8) & 0xF));
    e[59] = (UCHAR)h; e[60] = (UCHAR)vb; e[61] = (UCHAR)(((h >> 8) & 0xF) << 4 | ((vb >> 8) & 0xF));
    e[62] = 16; e[63] = 32; e[64] = 0x31; e[65] = 0;
    e[71] = 0x1E;
    // Descriptor 2: the monitor's name.
    e[75] = 0xFC;
    RtlCopyMemory(e + 77, name, 13);
    for (i = 0; i < 127; i++) sum += e[i];
    e[127] = (UCHAR)(256 - (sum & 0xFF));
}

// -----| device |-----

static NTSTATUS AddDevice(PDEVICE_OBJECT pdo, PVOID *ctx) {
    K3 *k = (K3 *)ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof(K3), '3kdV');
    if (!k) return STATUS_NO_MEMORY;
    k->Pdo = pdo;
    *ctx = k;
    LOG("added (PDO %p)", pdo);
    return STATUS_SUCCESS;
}

static NTSTATUS StartDevice(PVOID ctx, PDXGK_START_INFO start, PDXGKRNL_INTERFACE dxgk, PULONG sources, PULONG children) {
    K3 *k = (K3 *)ctx;
    ULONG got = 0;
    NTSTATUS st;
    UNREFERENCED_PARAMETER(start);
    k->Dxgk = *dxgk;
    st = k->Dxgk.DxgkCbGetDeviceInformation(k->Dxgk.DeviceHandle, &k->DeviceInfo);
    if (!NT_SUCCESS(st)) return st;
    k->Dxgk.DxgkCbReadDeviceSpace(k->Dxgk.DeviceHandle, DXGK_WHICHSPACE_CONFIG, &k->Vendor, 0, 2, &got);
    k->Dxgk.DxgkCbReadDeviceSpace(k->Dxgk.DeviceHandle, DXGK_WHICHSPACE_CONFIG, &k->Device, 2, 2, &got);
    LOG("starting on PCI %04x:%04x, PDO %p (dxgkrnl says %p), %u resource list(s), %llu MiB of RAM, key %wZ",
        k->Vendor, k->Device, k->Pdo, k->DeviceInfo.PhysicalDeviceObject,
        k->DeviceInfo.TranslatedResourceList ? k->DeviceInfo.TranslatedResourceList->Count : 0,
        k->DeviceInfo.SystemMemorySize.QuadPart >> 20, &k->DeviceInfo.DeviceRegistryPath);
    st = k->Dxgk.DxgkCbAcquirePostDisplayOwnership(k->Dxgk.DeviceHandle, &k->Post);
    if (!NT_SUCCESS(st) || k->Post.Width == 0 || !Bpp(k->Post.ColorFormat)) {
        LOG("no POST display to drive (%08x, %ux%u format %u)", st, k->Post.Width, k->Post.Height, k->Post.ColorFormat);
        return STATUS_UNSUCCESSFUL;
    }
    st = k->Dxgk.DxgkCbMapMemory(k->Dxgk.DeviceHandle, k->Post.PhysicAddress, k->Post.Pitch * k->Post.Height, FALSE, FALSE, MmWriteCombined, (PVOID *)&k->Fb);
    if (!NT_SUCCESS(st)) return st;
    LOG("POST display %ux%u pitch %u format %u at %llx, mapped at %p", k->Post.Width, k->Post.Height, k->Post.Pitch,
        k->Post.ColorFormat, k->Post.PhysicAddress.QuadPart, k->Fb);
    MakeEdid(k);
    *sources = 1;
    *children = 1;
    return STATUS_SUCCESS;
}

static NTSTATUS StopDevice(PVOID ctx) {
    K3 *k = (K3 *)ctx;
    if (k->Fb) k->Dxgk.DxgkCbUnmapMemory(k->Dxgk.DeviceHandle, k->Fb);
    k->Fb = NULL;
    return STATUS_SUCCESS;
}

static NTSTATUS StopAndRelease(PVOID ctx, D3DDDI_VIDEO_PRESENT_TARGET_ID target, PDXGK_DISPLAY_INFORMATION info) {
    K3 *k = (K3 *)ctx;
    UNREFERENCED_PARAMETER(target);
    *info = k->Post;
    LOG("stopping: %u presents, %u rectangles, %llu pixels copied; display handed back", k->Presents, k->Rects, k->Pixels);
    return StopDevice(ctx);
}

static NTSTATUS RemoveDevice(PVOID ctx) {
    ExFreePoolWithTag(ctx, '3kdV');
    return STATUS_SUCCESS;
}

static void Unload(void) {
    LOG("unloaded%s", "");
}

static NTSTATUS SetPowerState(PVOID ctx, ULONG uid, DEVICE_POWER_STATE state, POWER_ACTION action) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(uid); UNREFERENCED_PARAMETER(state); UNREFERENCED_PARAMETER(action);
    return STATUS_SUCCESS;
}

static void ResetDevice(PVOID ctx) {
    UNREFERENCED_PARAMETER(ctx);
}

static NTSTATUS DispatchIoRequest(PVOID ctx, ULONG src, PVIDEO_REQUEST_PACKET rp) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(src); UNREFERENCED_PARAMETER(rp);
    return STATUS_NOT_SUPPORTED;
}

static NTSTATUS QueryInterface(PVOID ctx, PQUERY_INTERFACE qi) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(qi);
    return STATUS_NOT_SUPPORTED;
}

static NTSTATUS QueryAdapterInfo(HANDLE ctx, const DXGKARG_QUERYADAPTERINFO *qai) {
    UNREFERENCED_PARAMETER(ctx);
    if (qai->Type == DXGKQAITYPE_DRIVERCAPS && qai->OutputDataSize >= sizeof(DXGK_DRIVERCAPS)) {
        DXGK_DRIVERCAPS *caps = (DXGK_DRIVERCAPS *)qai->pOutputData;
        RtlZeroMemory(caps, sizeof *caps);
        caps->WDDMVersion = DXGKDDI_WDDMv1_2;
        caps->HighestAcceptableAddress.QuadPart = -1;
        caps->SupportNonVGA = TRUE;
        return STATUS_SUCCESS;
    }
    return STATUS_NOT_SUPPORTED;
}

// -----| children and the monitor |-----

static NTSTATUS QueryChildRelations(PVOID ctx, PDXGK_CHILD_DESCRIPTOR rel, ULONG size) {
    UNREFERENCED_PARAMETER(ctx);
    if (size < 2 * sizeof(DXGK_CHILD_DESCRIPTOR)) return STATUS_BUFFER_TOO_SMALL;
    rel[0].ChildDeviceType = TypeVideoOutput;
    rel[0].ChildCapabilities.HpdAwareness = HpdAwarenessAlwaysConnected;
    rel[0].ChildCapabilities.Type.VideoOutput.InterfaceTechnology = D3DKMDT_VOT_INTERNAL;
    rel[0].ChildCapabilities.Type.VideoOutput.MonitorOrientationAwareness = D3DKMDT_MOA_NONE;
    rel[0].ChildCapabilities.Type.VideoOutput.SupportsSdtvModes = FALSE;
    rel[0].AcpiUid = 0;
    rel[0].ChildUid = 0;
    return STATUS_SUCCESS;
}

static NTSTATUS QueryChildStatus(PVOID ctx, PDXGK_CHILD_STATUS cs, BOOLEAN nondestructive) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(nondestructive);
    if (cs->ChildUid != 0) return STATUS_INVALID_PARAMETER;
    if (cs->Type == StatusConnection) cs->HotPlug.Connected = TRUE;
    return STATUS_SUCCESS;
}

static NTSTATUS QueryDeviceDescriptor(PVOID ctx, ULONG uid, PDXGK_DEVICE_DESCRIPTOR d) {
    K3 *k = (K3 *)ctx;
    if (uid != 0) return STATUS_INVALID_PARAMETER;
    if (d->DescriptorOffset >= 128) return STATUS_MONITOR_NO_MORE_DESCRIPTOR_DATA;
    RtlCopyMemory(d->DescriptorBuffer, k->Edid + d->DescriptorOffset, min(d->DescriptorLength, 128 - d->DescriptorOffset));
    return STATUS_SUCCESS;
}

static NTSTATUS RecommendMonitorModes(HANDLE ctx, const DXGKARG_RECOMMENDMONITORMODES *a) {
    K3 *k = (K3 *)ctx;
    D3DKMDT_MONITOR_SOURCE_MODE *m = NULL;
    NTSTATUS st = a->pMonitorSourceModeSetInterface->pfnCreateNewModeInfo(a->hMonitorSourceModeSet, &m);
    if (!NT_SUCCESS(st)) return st;
    m->VideoSignalInfo.VideoStandard = D3DKMDT_VSS_OTHER;
    m->VideoSignalInfo.TotalSize.cx = m->VideoSignalInfo.ActiveSize.cx = k->Post.Width;
    m->VideoSignalInfo.TotalSize.cy = m->VideoSignalInfo.ActiveSize.cy = k->Post.Height;
    m->VideoSignalInfo.VSyncFreq.Numerator = 60;
    m->VideoSignalInfo.VSyncFreq.Denominator = 1;
    m->VideoSignalInfo.HSyncFreq.Numerator = 60 * k->Post.Height;
    m->VideoSignalInfo.HSyncFreq.Denominator = 1;
    m->VideoSignalInfo.PixelRate = 60 * k->Post.Width * k->Post.Height;
    m->VideoSignalInfo.ScanLineOrdering = D3DDDI_VSSLO_PROGRESSIVE;
    m->ColorBasis = D3DKMDT_CB_SRGB;
    m->ColorCoeffDynamicRanges.FirstChannel = m->ColorCoeffDynamicRanges.SecondChannel = m->ColorCoeffDynamicRanges.ThirdChannel = 8;
    m->Origin = D3DKMDT_MCO_DRIVER;
    m->Preference = D3DKMDT_MP_NOTPREFERRED;
    st = a->pMonitorSourceModeSetInterface->pfnAddMode(a->hMonitorSourceModeSet, m);
    if (!NT_SUCCESS(st)) a->pMonitorSourceModeSetInterface->pfnReleaseModeInfo(a->hMonitorSourceModeSet, m);
    LOG("recommended monitor mode %ux%u: %08x", k->Post.Width, k->Post.Height, st);
    return STATUS_SUCCESS;
}

// -----| VidPN |-----

static const DXGK_VIDPN_INTERFACE *VidPn(K3 *k, D3DKMDT_HVIDPN h) {
    const DXGK_VIDPN_INTERFACE *v = NULL;
    if (!NT_SUCCESS(k->Dxgk.DxgkCbQueryVidPnInterface(h, DXGK_VIDPN_INTERFACE_VERSION_V1, &v))) return NULL;
    return v;
}

// Whether the pinned source mode (if any) of `src` is the POST mode.
static BOOLEAN SourceOk(K3 *k, const DXGK_VIDPN_INTERFACE *v, D3DKMDT_HVIDPN h, D3DDDI_VIDEO_PRESENT_SOURCE_ID src) {
    D3DKMDT_HVIDPNSOURCEMODESET set;
    const DXGK_VIDPNSOURCEMODESET_INTERFACE *si;
    const D3DKMDT_VIDPN_SOURCE_MODE *pinned = NULL;
    BOOLEAN ok = TRUE;
    if (!NT_SUCCESS(v->pfnAcquireSourceModeSet(h, src, &set, &si))) return FALSE;
    if (NT_SUCCESS(si->pfnAcquirePinnedModeInfo(set, &pinned)) && pinned) {
        ok = pinned->Type == D3DKMDT_RMT_GRAPHICS && pinned->Format.Graphics.PrimSurfSize.cx == k->Post.Width &&
             pinned->Format.Graphics.PrimSurfSize.cy == k->Post.Height;
        si->pfnReleaseModeInfo(set, pinned);
    }
    v->pfnReleaseSourceModeSet(h, set);
    return ok;
}

static NTSTATUS IsSupportedVidPn(HANDLE ctx, DXGKARG_ISSUPPORTEDVIDPN *a) {
    K3 *k = (K3 *)ctx;
    const DXGK_VIDPN_INTERFACE *v = VidPn(k, a->hDesiredVidPn);
    D3DKMDT_HVIDPNTOPOLOGY topo;
    const DXGK_VIDPNTOPOLOGY_INTERFACE *ti;
    SIZE_T n = 0;
    a->IsVidPnSupported = FALSE;
    if (!v || !NT_SUCCESS(v->pfnGetTopology(a->hDesiredVidPn, &topo, &ti))) return STATUS_SUCCESS;
    if (!NT_SUCCESS(ti->pfnGetNumPaths(topo, &n))) return STATUS_SUCCESS;
    // One output at most, from source 0, showing the POST mode.
    if (n == 0) {
        a->IsVidPnSupported = TRUE;
    } else if (n == 1) {
        D3DDDI_VIDEO_PRESENT_TARGET_ID t = 0;
        a->IsVidPnSupported = NT_SUCCESS(ti->pfnEnumPathTargetsFromSource(topo, 0, 0, &t)) && t == 0 && SourceOk(k, v, a->hDesiredVidPn, 0);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS AddSourceMode(K3 *k, const DXGK_VIDPN_INTERFACE *v, D3DKMDT_HVIDPN h) {
    D3DKMDT_HVIDPNSOURCEMODESET set;
    const DXGK_VIDPNSOURCEMODESET_INTERFACE *si;
    D3DKMDT_VIDPN_SOURCE_MODE *m = NULL;
    NTSTATUS st = v->pfnCreateNewSourceModeSet(h, 0, &set, &si);
    if (!NT_SUCCESS(st)) return st;
    st = si->pfnCreateNewModeInfo(set, &m);
    if (NT_SUCCESS(st)) {
        m->Type = D3DKMDT_RMT_GRAPHICS;
        m->Format.Graphics.PrimSurfSize.cx = m->Format.Graphics.VisibleRegionSize.cx = k->Post.Width;
        m->Format.Graphics.PrimSurfSize.cy = m->Format.Graphics.VisibleRegionSize.cy = k->Post.Height;
        m->Format.Graphics.Stride = k->Post.Pitch;
        m->Format.Graphics.PixelFormat = D3DDDIFMT_A8R8G8B8;
        m->Format.Graphics.ColorBasis = D3DKMDT_CB_SCRGB;
        m->Format.Graphics.PixelValueAccessMode = D3DKMDT_PVAM_DIRECT;
        st = si->pfnAddMode(set, m);
        if (!NT_SUCCESS(st)) si->pfnReleaseModeInfo(set, m);
    }
    if (NT_SUCCESS(st)) st = v->pfnAssignSourceModeSet(h, 0, set);
    if (!NT_SUCCESS(st)) v->pfnReleaseSourceModeSet(h, set);
    return st;
}

static NTSTATUS AddTargetMode(K3 *k, const DXGK_VIDPN_INTERFACE *v, D3DKMDT_HVIDPN h) {
    D3DKMDT_HVIDPNTARGETMODESET set;
    const DXGK_VIDPNTARGETMODESET_INTERFACE *ti;
    D3DKMDT_VIDPN_TARGET_MODE *m = NULL;
    NTSTATUS st = v->pfnCreateNewTargetModeSet(h, 0, &set, &ti);
    if (!NT_SUCCESS(st)) return st;
    st = ti->pfnCreateNewModeInfo(set, &m);
    if (NT_SUCCESS(st)) {
        m->VideoSignalInfo.VideoStandard = D3DKMDT_VSS_OTHER;
        m->VideoSignalInfo.TotalSize.cx = m->VideoSignalInfo.ActiveSize.cx = k->Post.Width;
        m->VideoSignalInfo.TotalSize.cy = m->VideoSignalInfo.ActiveSize.cy = k->Post.Height;
        m->VideoSignalInfo.VSyncFreq.Numerator = D3DKMDT_FREQUENCY_NOTSPECIFIED;
        m->VideoSignalInfo.VSyncFreq.Denominator = D3DKMDT_FREQUENCY_NOTSPECIFIED;
        m->VideoSignalInfo.HSyncFreq.Numerator = D3DKMDT_FREQUENCY_NOTSPECIFIED;
        m->VideoSignalInfo.HSyncFreq.Denominator = D3DKMDT_FREQUENCY_NOTSPECIFIED;
        m->VideoSignalInfo.PixelRate = D3DKMDT_FREQUENCY_NOTSPECIFIED;
        m->VideoSignalInfo.ScanLineOrdering = D3DDDI_VSSLO_PROGRESSIVE;
        m->Preference = D3DKMDT_MP_PREFERRED;
        st = ti->pfnAddMode(set, m);
        if (!NT_SUCCESS(st)) ti->pfnReleaseModeInfo(set, m);
    }
    if (NT_SUCCESS(st)) st = v->pfnAssignTargetModeSet(h, 0, set);
    if (!NT_SUCCESS(st)) v->pfnReleaseTargetModeSet(h, set);
    return st;
}

static NTSTATUS EnumVidPnCofuncModality(HANDLE ctx, const DXGKARG_ENUMVIDPNCOFUNCMODALITY *a) {
    K3 *k = (K3 *)ctx;
    const DXGK_VIDPN_INTERFACE *v = VidPn(k, a->hConstrainingVidPn);
    D3DKMDT_HVIDPNTOPOLOGY topo;
    const DXGK_VIDPNTOPOLOGY_INTERFACE *ti;
    const D3DKMDT_VIDPN_PRESENT_PATH *path = NULL;
    NTSTATUS st;
    if (!v) return STATUS_INVALID_PARAMETER;
    st = v->pfnGetTopology(a->hConstrainingVidPn, &topo, &ti);
    if (!NT_SUCCESS(st)) return st;
    st = ti->pfnAcquireFirstPathInfo(topo, &path);
    if (st == STATUS_GRAPHICS_DATASET_IS_EMPTY) return STATUS_SUCCESS;
    if (!NT_SUCCESS(st)) return st;
    // Sources and targets: the POST mode, unless one is pinned already.
    {
        D3DKMDT_HVIDPNSOURCEMODESET ss;
        const DXGK_VIDPNSOURCEMODESET_INTERFACE *si;
        const D3DKMDT_VIDPN_SOURCE_MODE *pinned = NULL;
        if (NT_SUCCESS(v->pfnAcquireSourceModeSet(a->hConstrainingVidPn, path->VidPnSourceId, &ss, &si))) {
            si->pfnAcquirePinnedModeInfo(ss, &pinned);
            if (pinned) si->pfnReleaseModeInfo(ss, pinned);
            v->pfnReleaseSourceModeSet(a->hConstrainingVidPn, ss);
            if (!pinned) st = AddSourceMode(k, v, a->hConstrainingVidPn);
        }
    }
    if (NT_SUCCESS(st)) {
        D3DKMDT_HVIDPNTARGETMODESET ts;
        const DXGK_VIDPNTARGETMODESET_INTERFACE *tsi;
        const D3DKMDT_VIDPN_TARGET_MODE *pinned = NULL;
        if (NT_SUCCESS(v->pfnAcquireTargetModeSet(a->hConstrainingVidPn, path->VidPnTargetId, &ts, &tsi))) {
            tsi->pfnAcquirePinnedModeInfo(ts, &pinned);
            if (pinned) tsi->pfnReleaseModeInfo(ts, pinned);
            v->pfnReleaseTargetModeSet(a->hConstrainingVidPn, ts);
            if (!pinned) st = AddTargetMode(k, v, a->hConstrainingVidPn);
        }
    }
    // Only identity scaling and rotation.
    if (NT_SUCCESS(st)) {
        D3DKMDT_VIDPN_PRESENT_PATH p = *path;
        RtlZeroMemory(&p.ContentTransformation.ScalingSupport, sizeof p.ContentTransformation.ScalingSupport);
        RtlZeroMemory(&p.ContentTransformation.RotationSupport, sizeof p.ContentTransformation.RotationSupport);
        p.ContentTransformation.ScalingSupport.Identity = 1;
        p.ContentTransformation.RotationSupport.Identity = 1;
        st = ti->pfnUpdatePathSupportInfo(topo, &p);
    }
    ti->pfnReleasePathInfo(topo, path);
    LOG("cofunctional modes (pivot %u): %08x", a->EnumPivotType, st);
    return st;
}

static NTSTATUS CommitVidPn(HANDLE ctx, const DXGKARG_COMMITVIDPN *a) {
    K3 *k = (K3 *)ctx;
    const DXGK_VIDPN_INTERFACE *v = VidPn(k, a->hFunctionalVidPn);
    D3DKMDT_HVIDPNSOURCEMODESET set;
    const DXGK_VIDPNSOURCEMODESET_INTERFACE *si;
    const D3DKMDT_VIDPN_SOURCE_MODE *pinned = NULL;
    NTSTATUS st;
    if (!v) return STATUS_INVALID_PARAMETER;
    st = v->pfnAcquireSourceModeSet(a->hFunctionalVidPn, 0, &set, &si);
    if (!NT_SUCCESS(st)) return st;
    si->pfnAcquirePinnedModeInfo(set, &pinned);
    if (pinned) {
        k->Committed = pinned->Format.Graphics.PrimSurfSize.cx == k->Post.Width && pinned->Format.Graphics.PrimSurfSize.cy == k->Post.Height;
        LOG("committed %ux%u (%s)", pinned->Format.Graphics.PrimSurfSize.cx, pinned->Format.Graphics.PrimSurfSize.cy,
            k->Committed ? "the POST mode" : "NOT the POST mode");
        si->pfnReleaseModeInfo(set, pinned);
    } else {
        k->Committed = FALSE;
        LOG("committed: no mode (display off)%s", "");
    }
    v->pfnReleaseSourceModeSet(a->hFunctionalVidPn, set);
    return k->Committed || !pinned ? STATUS_SUCCESS : STATUS_GRAPHICS_INVALID_VIDPN;
}

static NTSTATUS SetVidPnSourceVisibility(HANDLE ctx, const DXGKARG_SETVIDPNSOURCEVISIBILITY *a) {
    K3 *k = (K3 *)ctx;
    k->Visible = a->Visible;
    if (!a->Visible && k->Fb) RtlZeroMemory(k->Fb, k->Post.Pitch * k->Post.Height);
    LOG("source %u %s", a->VidPnSourceId, a->Visible ? "visible" : "hidden");
    return STATUS_SUCCESS;
}

static NTSTATUS UpdateActiveVidPnPresentPath(HANDLE ctx, const DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH *a) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(a);
    return STATUS_SUCCESS;
}

static NTSTATUS QueryVidPnHWCapability(HANDLE ctx, DXGKARG_QUERYVIDPNHWCAPABILITY *a) {
    UNREFERENCED_PARAMETER(ctx);
    RtlZeroMemory(&a->VidPnHWCaps, sizeof a->VidPnHWCaps);
    return STATUS_SUCCESS;
}

static NTSTATUS SetPointerPosition(HANDLE ctx, const DXGKARG_SETPOINTERPOSITION *a) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(a);
    return STATUS_NOT_SUPPORTED;
}

static NTSTATUS SetPointerShape(HANDLE ctx, const DXGKARG_SETPOINTERSHAPE *a) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(a);
    return STATUS_NOT_SUPPORTED;
}

static NTSTATUS GetScanLine(HANDLE ctx, DXGKARG_GETSCANLINE *a) {
    UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(a);
    return STATUS_NOT_SUPPORTED;
}

// -----| presenting |-----

static void CopyRect(K3 *k, const BYTE *src, LONG pitch, const RECT *r) {
    LONG y, w;
    LONG right = min(r->right, (LONG)k->Post.Width), bottom = min(r->bottom, (LONG)k->Post.Height);
    if (r->left < 0 || r->top < 0 || r->left >= right || r->top >= bottom) return;
    w = (right - r->left) * 4;
    for (y = r->top; y < bottom; y++)
        RtlCopyMemory(k->Fb + y * k->Post.Pitch + r->left * 4, src + y * pitch + r->left * 4, w);
    k->Rects++;
    k->Pixels += (ULONGLONG)(right - r->left) * (bottom - r->top);
}

static NTSTATUS PresentDisplayOnly(HANDLE ctx, const DXGKARG_PRESENT_DISPLAYONLY *a) {
    K3 *k = (K3 *)ctx;
    ULONG i;
    if (!k->Fb || a->BytesPerPixel != 4 || !k->Visible) return STATUS_SUCCESS;
    for (i = 0; i < a->NumMoves; i++) {
        // A move: the destination from the source point, in the framebuffer.
        const D3DKMT_MOVE_RECT *m = &a->pMoves[i];
        RECT d = m->DestRect;
        LONG dy = m->SourcePoint.y - d.top, dx = m->SourcePoint.x - d.left, y;
        LONG w = (d.right - d.left) * 4;
        if (dy >= 0) {
            for (y = d.top; y < d.bottom; y++)
                RtlMoveMemory(k->Fb + y * k->Post.Pitch + d.left * 4, k->Fb + (y + dy) * k->Post.Pitch + (d.left + dx) * 4, w);
        } else {
            for (y = d.bottom - 1; y >= d.top; y--)
                RtlMoveMemory(k->Fb + y * k->Post.Pitch + d.left * 4, k->Fb + (y + dy) * k->Post.Pitch + (d.left + dx) * 4, w);
        }
    }
    for (i = 0; i < a->NumDirtyRects; i++) CopyRect(k, (const BYTE *)a->pSource, a->Pitch, &a->pDirtyRect[i]);
    k->Presents++;
    if (k->Presents == 1 || k->Presents % 200 == 0)
        LOG("present %u: %u rectangle(s), first (%d,%d)-(%d,%d), %llu pixels so far", k->Presents, a->NumDirtyRects,
            a->NumDirtyRects ? a->pDirtyRect[0].left : 0, a->NumDirtyRects ? a->pDirtyRect[0].top : 0,
            a->NumDirtyRects ? a->pDirtyRect[0].right : 0, a->NumDirtyRects ? a->pDirtyRect[0].bottom : 0, k->Pixels);
    return STATUS_SUCCESS;
}

// The bug-check screen: dxgkrnl draws through these when the system stops.
static NTSTATUS SystemDisplayEnable(PVOID ctx, D3DDDI_VIDEO_PRESENT_TARGET_ID target, PDXGKARG_SYSTEM_DISPLAY_ENABLE_FLAGS flags,
                                    UINT *w, UINT *h, D3DDDIFORMAT *fmt) {
    K3 *k = (K3 *)ctx;
    UNREFERENCED_PARAMETER(target); UNREFERENCED_PARAMETER(flags);
    *w = k->Post.Width;
    *h = k->Post.Height;
    *fmt = D3DDDIFMT_X8R8G8B8;
    return STATUS_SUCCESS;
}

static void SystemDisplayWrite(PVOID ctx, PVOID src, UINT sw, UINT sh, UINT stride, UINT x, UINT y) {
    K3 *k = (K3 *)ctx;
    RECT r = { 0, 0, (LONG)sw, (LONG)sh };
    UINT row;
    UNREFERENCED_PARAMETER(r);
    for (row = 0; row < sh && y + row < k->Post.Height; row++)
        RtlCopyMemory(k->Fb + (y + row) * k->Post.Pitch + x * 4, (BYTE *)src + row * stride, min(sw, k->Post.Width - x) * 4);
}

NTSTATUS DriverEntry(PDRIVER_OBJECT drv, PUNICODE_STRING reg) {
    KMDDOD_INITIALIZATION_DATA init;
    NTSTATUS st;
    RtlZeroMemory(&init, sizeof init);
    init.Version = DXGKDDI_INTERFACE_VERSION;
    init.DxgkDdiAddDevice = AddDevice;
    init.DxgkDdiStartDevice = StartDevice;
    init.DxgkDdiStopDevice = StopDevice;
    init.DxgkDdiResetDevice = ResetDevice;
    init.DxgkDdiRemoveDevice = RemoveDevice;
    init.DxgkDdiDispatchIoRequest = DispatchIoRequest;
    init.DxgkDdiQueryInterface = QueryInterface;
    init.DxgkDdiQueryChildRelations = QueryChildRelations;
    init.DxgkDdiQueryChildStatus = QueryChildStatus;
    init.DxgkDdiQueryDeviceDescriptor = QueryDeviceDescriptor;
    init.DxgkDdiSetPowerState = SetPowerState;
    init.DxgkDdiUnload = Unload;
    init.DxgkDdiQueryAdapterInfo = QueryAdapterInfo;
    init.DxgkDdiSetPointerPosition = SetPointerPosition;
    init.DxgkDdiSetPointerShape = SetPointerShape;
    init.DxgkDdiIsSupportedVidPn = IsSupportedVidPn;
    init.DxgkDdiRecommendFunctionalVidPn = NULL;
    init.DxgkDdiEnumVidPnCofuncModality = EnumVidPnCofuncModality;
    init.DxgkDdiSetVidPnSourceVisibility = SetVidPnSourceVisibility;
    init.DxgkDdiCommitVidPn = CommitVidPn;
    init.DxgkDdiUpdateActiveVidPnPresentPath = UpdateActiveVidPnPresentPath;
    init.DxgkDdiRecommendMonitorModes = RecommendMonitorModes;
    init.DxgkDdiGetScanLine = GetScanLine;
    init.DxgkDdiQueryVidPnHWCapability = QueryVidPnHWCapability;
    init.DxgkDdiPresentDisplayOnly = PresentDisplayOnly;
    init.DxgkDdiStopDeviceAndReleasePostDisplayOwnership = StopAndRelease;
    init.DxgkDdiSystemDisplayEnable = SystemDisplayEnable;
    init.DxgkDdiSystemDisplayWrite = SystemDisplayWrite;
    st = DxgkInitializeDisplayOnlyDriver(drv, reg, &init);
    LOG("DxgkInitializeDisplayOnlyDriver: %08x (DDI version %x)", st, DXGKDDI_INTERFACE_VERSION);
    return st;
}
