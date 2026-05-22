// SPDX-License-Identifier: BSD-3-Clause
// Deep scanline -> RGBA32F flatten (Imf C++ API) for Simple Image Viewer.
// Composites samples front-to-back using straight-alpha "over" sorted by increasing Z.
//
// File access: prefer the *_bytes entry points fed from a Rust memory map so Unicode
// paths never cross the FFI boundary as encoded bytes. Path-based entry points remain
// for callers that already have a UTF-8 path (OpenEXR 3.x ImfIO contract).

#include <OpenEXR/IexBaseExc.h>
#include <OpenEXR/ImfArray.h>
#include <OpenEXR/ImfChannelList.h>
#include <OpenEXR/ImfChromaticitiesAttribute.h>
#include <OpenEXR/ImfDeepFrameBuffer.h>
#include <OpenEXR/ImfDeepScanLineInputFile.h>
#include <OpenEXR/ImfFrameBuffer.h>
#include <OpenEXR/ImfInputFile.h>
#include <OpenEXR/ImfIO.h>
#include <OpenEXR/ImfRgbaFile.h>
#include <OpenEXR/ImfStdIO.h>
#include <half.h>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

using namespace Imf;

static float
halfToFloat (half h)
{
    return static_cast<float> (h);
}

/// Read-only memory buffer exposed as Imf::IStream (Unicode-safe when fed from Rust mmap).
class SivMemoryIStream : public IStream
{
public:
    SivMemoryIStream (const void* data, std::size_t size, const char* debugName)
        : IStream (debugName && debugName[0] ? debugName : "<memory>")
        , _data (static_cast<const char*> (data))
        , _size (size)
        , _pos (0)
    {}

    bool isMemoryMapped () const override { return true; }

    bool read (char c[/*n*/], int n) override
    {
        if (n <= 0)
            return _pos < _size;
        const auto need = static_cast<std::size_t> (n);
        if (_pos + need > _size)
            throw Iex::InputExc ("Unexpected end of file");
        std::memcpy (c, _data + _pos, need);
        _pos += need;
        return _pos < _size;
    }

    char* readMemoryMapped (int n) override
    {
        if (n <= 0)
            throw Iex::InputExc ("Invalid read size");
        const auto need = static_cast<std::size_t> (n);
        if (_pos + need > _size)
            throw Iex::InputExc ("Unexpected end of file");
        char* p = const_cast<char*> (_data + _pos);
        _pos += need;
        return p;
    }

    uint64_t tellg () override { return _pos; }

    void seekg (uint64_t pos) override
    {
        if (pos > _size)
            throw Iex::InputExc ("Invalid seek position");
        _pos = pos;
    }

    int64_t size () override { return static_cast<int64_t> (_size); }

private:
    const char* _data;
    std::size_t _size;
    std::size_t _pos;
};

/// Owns per-pixel deep sample arrays; pointer grids are non-owning views for Imf::DeepFrameBuffer.
class DeepScanlineSampleStore
{
public:
    void resizeErase (int height, int width)
    {
        _r.resizeErase (height, width);
        _g.resizeErase (height, width);
        _b.resizeErase (height, width);
        _ah.resizeErase (height, width);
        _af.resizeErase (height, width);
        _z.resizeErase (height, width);
        _rPtr.resizeErase (height, width);
        _gPtr.resizeErase (height, width);
        _bPtr.resizeErase (height, width);
        _ahPtr.resizeErase (height, width);
        _afPtr.resizeErase (height, width);
        _zPtr.resizeErase (height, width);
    }

    void allocatePixel (
        int y, int x, unsigned int n, bool wantHalfA, bool wantFloatA, bool wantZ)
    {
        if (n == 0)
            return;
        _r[y][x] = std::make_unique<float[]> (n);
        _g[y][x] = std::make_unique<float[]> (n);
        _b[y][x] = std::make_unique<float[]> (n);
        _rPtr[y][x] = _r[y][x].get ();
        _gPtr[y][x] = _g[y][x].get ();
        _bPtr[y][x] = _b[y][x].get ();
        if (wantHalfA)
        {
            _ah[y][x] = std::make_unique<half[]> (n);
            _ahPtr[y][x] = _ah[y][x].get ();
        }
        if (wantFloatA)
        {
            _af[y][x] = std::make_unique<float[]> (n);
            _afPtr[y][x] = _af[y][x].get ();
        }
        if (wantZ)
        {
            _z[y][x] = std::make_unique<float[]> (n);
            _zPtr[y][x] = _z[y][x].get ();
        }
    }

    Array2D<float*>& rPtr () { return _rPtr; }
    Array2D<float*>& gPtr () { return _gPtr; }
    Array2D<float*>& bPtr () { return _bPtr; }
    Array2D<half*>& ahPtr () { return _ahPtr; }
    Array2D<float*>& afPtr () { return _afPtr; }
    Array2D<float*>& zPtr () { return _zPtr; }

    float* r (int y, int x) const { return _rPtr[y][x]; }
    float* g (int y, int x) const { return _gPtr[y][x]; }
    float* b (int y, int x) const { return _bPtr[y][x]; }
    half* ah (int y, int x) const { return _ahPtr[y][x]; }
    float* af (int y, int x) const { return _afPtr[y][x]; }
    float* z (int y, int x) const { return _zPtr[y][x]; }

private:
    Array2D<std::unique_ptr<float[]>> _r;
    Array2D<std::unique_ptr<float[]>> _g;
    Array2D<std::unique_ptr<float[]>> _b;
    Array2D<std::unique_ptr<half[]>> _ah;
    Array2D<std::unique_ptr<float[]>> _af;
    Array2D<std::unique_ptr<float[]>> _z;
    Array2D<float*> _rPtr;
    Array2D<float*> _gPtr;
    Array2D<float*> _bPtr;
    Array2D<half*> _ahPtr;
    Array2D<float*> _afPtr;
    Array2D<float*> _zPtr;
};

static int
deep_scanline_flatten_impl (
    IStream& is, float* out_rgba, std::size_t out_len, unsigned* out_w, unsigned* out_h)
{
    DeepScanLineInputFile file (is);

    const Header& header = file.header ();
    if (header.type () != "deepscanline")
        return -3;

    const ChannelList& chans = header.channels ();
    if (!chans.findChannel ("R") || !chans.findChannel ("G") || !chans.findChannel ("B"))
        return -4;
    const Channel* chanA = chans.findChannel ("A");
    enum class AStorage
    {
        None,
        Half,
        Float
    } aStorage = AStorage::None;
    if (chanA)
    {
        if (chanA->type == HALF)
            aStorage = AStorage::Half;
        else if (chanA->type == FLOAT)
            aStorage = AStorage::Float;
        else
            return -6;
    }
    const bool hasZ = chans.findChannel ("Z") != nullptr;

    Imath::Box2i dw = header.dataWindow ();
    int xmin     = dw.min.x;
    int ymin     = dw.min.y;
    int xmax     = dw.max.x;
    int ymax     = dw.max.y;
    int width    = xmax - xmin + 1;
    int height   = ymax - ymin + 1;
    std::size_t need = static_cast<std::size_t> (width) * static_cast<std::size_t> (height) * 4u;
    if (out_len < need)
        return -5;

    Array2D<unsigned int> sampleCount;
    sampleCount.resizeErase (height, width);

    DeepScanlineSampleStore samples;
    samples.resizeErase (height, width);

    DeepFrameBuffer fb;

    char* scBase = reinterpret_cast<char*> (&sampleCount[0][0]);
    char* scPtr =
        scBase - sizeof (unsigned int) * xmin - sizeof (unsigned int) * ymin * width;

    fb.insertSampleCountSlice (Slice (
        PixelType::UINT,
        scPtr,
        sizeof (unsigned int),
        sizeof (unsigned int) * width));

    auto addFloatChan = [&](const char* name, Array2D<float*>& arr) {
        char* base = reinterpret_cast<char*> (&arr[0][0]);
        char* ptr  = base - sizeof (float*) * xmin - sizeof (float*) * ymin * width;
        fb.insert (
            name,
            DeepSlice (
                PixelType::FLOAT,
                ptr,
                sizeof (float*),
                sizeof (float*) * width,
                sizeof (float)));
    };
    auto addHalfChan = [&](const char* name, Array2D<half*>& arr) {
        char* base = reinterpret_cast<char*> (&arr[0][0]);
        char* ptr  = base - sizeof (half*) * xmin - sizeof (half*) * ymin * width;
        fb.insert (
            name,
            DeepSlice (
                PixelType::HALF,
                ptr,
                sizeof (half*),
                sizeof (half*) * width,
                sizeof (half)));
    };

    addFloatChan ("R", samples.rPtr ());
    addFloatChan ("G", samples.gPtr ());
    addFloatChan ("B", samples.bPtr ());
    if (aStorage == AStorage::Half)
        addHalfChan ("A", samples.ahPtr ());
    else if (aStorage == AStorage::Float)
        addFloatChan ("A", samples.afPtr ());
    if (hasZ)
        addFloatChan ("Z", samples.zPtr ());

    file.setFrameBuffer (fb);
    file.readPixelSampleCounts (ymin, ymax);

    for (int y = 0; y < height; ++y)
    {
        for (int x = 0; x < width; ++x)
        {
            const unsigned int n = sampleCount[y][x];
            samples.allocatePixel (
                y,
                x,
                n,
                n != 0 && aStorage == AStorage::Half,
                n != 0 && aStorage == AStorage::Float,
                n != 0 && hasZ);
        }
    }

    file.readPixels (ymin, ymax);

    for (int y = 0; y < height; ++y)
    {
        for (int x = 0; x < width; ++x)
        {
            unsigned int n = sampleCount[y][x];
            std::size_t di =
                (static_cast<std::size_t> (y) * static_cast<std::size_t> (width) +
                 static_cast<std::size_t> (x)) *
                4u;

            if (n == 0)
            {
                out_rgba[di + 0] = 0.f;
                out_rgba[di + 1] = 0.f;
                out_rgba[di + 2] = 0.f;
                out_rgba[di + 3] = 1.f;
                continue;
            }

            float* r = samples.r (y, x);
            float* g = samples.g (y, x);
            float* b = samples.b (y, x);
            half* ah = samples.ah (y, x);
            float* af = samples.af (y, x);
            float* z = samples.z (y, x);

            std::vector<int> order (n);
            for (unsigned int i = 0; i < n; ++i)
                order[i] = static_cast<int> (i);

            if (z)
            {
                std::stable_sort (order.begin (), order.end (), [&](int i, int j) {
                    return z[i] < z[j];
                });
            }

            float cr = 0.f, cg = 0.f, cb = 0.f;
            for (unsigned int k = 0; k < n; ++k)
            {
                int i = order[k];
                float ai = 1.f;
                if (aStorage == AStorage::Half)
                    ai = std::clamp (halfToFloat (ah[i]), 0.f, 1.f);
                else if (aStorage == AStorage::Float)
                    ai = std::clamp (af[i], 0.f, 1.f);
                float ri     = r[i];
                float gi     = g[i];
                float bi     = b[i];
                cr           = ri * ai + cr * (1.f - ai);
                cg           = gi * ai + cg * (1.f - ai);
                cb           = bi * ai + cb * (1.f - ai);
            }

            out_rgba[di + 0] = cr;
            out_rgba[di + 1] = cg;
            out_rgba[di + 2] = cb;
            out_rgba[di + 3] = 1.f;
        }
    }

    *out_w = static_cast<unsigned> (width);
    *out_h = static_cast<unsigned> (height);
    return 0;
}

static int
input_file_chromaticities_impl (IStream& is, float* out_rg_bw_xy)
{
    InputFile file (is);
    const Header& h = file.header ();
    const ChromaticitiesAttribute* attr =
        h.findTypedAttribute<ChromaticitiesAttribute> ("chromaticities");
    if (!attr)
        return -2;
    const Chromaticities& c = attr->value ();
    out_rg_bw_xy[0] = static_cast<float> (c.red.x);
    out_rg_bw_xy[1] = static_cast<float> (c.red.y);
    out_rg_bw_xy[2] = static_cast<float> (c.green.x);
    out_rg_bw_xy[3] = static_cast<float> (c.green.y);
    out_rg_bw_xy[4] = static_cast<float> (c.blue.x);
    out_rg_bw_xy[5] = static_cast<float> (c.blue.y);
    out_rg_bw_xy[6] = static_cast<float> (c.white.x);
    out_rg_bw_xy[7] = static_cast<float> (c.white.y);
    return 0;
}

static int
rgba_input_scanline_flatten_impl (
    IStream& is, float* out_rgba, std::size_t out_len, unsigned* out_w, unsigned* out_h)
{
    RgbaInputFile file (is);
    const Header& header = file.header ();
    if (header.type () != "scanlineimage")
        return -4;

    Imath::Box2i dw = header.dataWindow ();
    int xmin       = dw.min.x;
    int ymin       = dw.min.y;
    int width      = dw.max.x - dw.min.x + 1;
    int height     = dw.max.y - dw.min.y + 1;
    std::size_t need =
        static_cast<std::size_t> (width) * static_cast<std::size_t> (height) * 4u;
    *out_w         = static_cast<unsigned> (width);
    *out_h         = static_cast<unsigned> (height);
    if (out_len < need)
        return -5;

    Array2D<Rgba> pixels (height, width);
    file.setFrameBuffer (&pixels[-ymin][-xmin], 1, width);
    file.readPixels (dw.min.y, dw.max.y);

    for (int y = 0; y < height; ++y)
    {
        for (int x = 0; x < width; ++x)
        {
            const Rgba& p = pixels[y][x];
            std::size_t di =
                (static_cast<std::size_t> (y) * static_cast<std::size_t> (width) +
                 static_cast<std::size_t> (x)) *
                4u;
            out_rgba[di + 0] = halfToFloat (p.r);
            out_rgba[di + 1] = halfToFloat (p.g);
            out_rgba[di + 2] = halfToFloat (p.b);
            out_rgba[di + 3] = halfToFloat (p.a);
        }
    }

    *out_w = static_cast<unsigned> (width);
    *out_h = static_cast<unsigned> (height);
    return 0;
}

#define SIV_IMF_TRY_BYTES(IMPL, ...)                                           \
    do                                                                         \
    {                                                                          \
        if (!data || data_len == 0)                                            \
            return -1;                                                         \
        try                                                                    \
        {                                                                      \
            SivMemoryIStream is (data, data_len, debug_name_utf8);            \
            return IMPL (is, __VA_ARGS__);                                     \
        }                                                                      \
        catch (...)                                                            \
        {                                                                      \
            return -2;                                                         \
        }                                                                      \
    } while (0)

#define SIV_IMF_TRY_PATH(IMPL, ...)                                            \
    do                                                                         \
    {                                                                          \
        if (!path)                                                             \
            return -1;                                                         \
        try                                                                    \
        {                                                                      \
            StdIFStream is (path);                                             \
            return IMPL (is, __VA_ARGS__);                                     \
        }                                                                      \
        catch (...)                                                            \
        {                                                                      \
            return -2;                                                         \
        }                                                                      \
    } while (0)

extern "C" int
siv_imf_deep_scanline_flatten_rgba_bytes (
    const void* data,
    std::size_t data_len,
    const char* debug_name_utf8,
    float* out_rgba,
    std::size_t out_len,
    unsigned* out_w,
    unsigned* out_h)
{
    if (!out_rgba || !out_w || !out_h)
        return -1;
    *out_w = 0;
    *out_h = 0;
    SIV_IMF_TRY_BYTES (deep_scanline_flatten_impl, out_rgba, out_len, out_w, out_h);
}

extern "C" int
siv_imf_deep_scanline_flatten_rgba (
    const char* path, float* out_rgba, std::size_t out_len, unsigned* out_w, unsigned* out_h)
{
    if (!out_rgba || !out_w || !out_h)
        return -1;
    *out_w = 0;
    *out_h = 0;
    SIV_IMF_TRY_PATH (deep_scanline_flatten_impl, out_rgba, out_len, out_w, out_h);
}

extern "C" int
siv_imf_input_file_chromaticities_f32_bytes (
    const void* data, std::size_t data_len, const char* debug_name_utf8, float* out_rg_bw_xy)
{
    SIV_IMF_TRY_BYTES (input_file_chromaticities_impl, out_rg_bw_xy);
}

extern "C" int
siv_imf_input_file_chromaticities_f32 (const char* path, float* out_rg_bw_xy)
{
    if (!path || !out_rg_bw_xy)
        return -1;
    SIV_IMF_TRY_PATH (input_file_chromaticities_impl, out_rg_bw_xy);
}

extern "C" int
siv_imf_rgba_input_scanline_flatten_rgba_bytes (
    const void* data,
    std::size_t data_len,
    const char* debug_name_utf8,
    float* out_rgba,
    std::size_t out_len,
    unsigned* out_w,
    unsigned* out_h)
{
    if (!out_rgba || !out_w || !out_h)
        return -1;
    *out_w = 0;
    *out_h = 0;
    SIV_IMF_TRY_BYTES (rgba_input_scanline_flatten_impl, out_rgba, out_len, out_w, out_h);
}

extern "C" int
siv_imf_rgba_input_scanline_flatten_rgba (
    const char* path, float* out_rgba, std::size_t out_len, unsigned* out_w, unsigned* out_h)
{
    if (!out_rgba || !out_w || !out_h)
        return -1;
    *out_w = 0;
    *out_h = 0;
    SIV_IMF_TRY_PATH (rgba_input_scanline_flatten_impl, out_rgba, out_len, out_w, out_h);
}
