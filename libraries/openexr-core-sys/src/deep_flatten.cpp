// SPDX-License-Identifier: BSD-3-Clause
// Deep scanline -> RGBA32F flatten (Imf C++ API) for Simple Image Viewer.
// Composites samples front-to-back using straight-alpha "over" sorted by increasing Z.

#include <OpenEXR/ImfArray.h>
#include <OpenEXR/ImfChannelList.h>
#include <OpenEXR/ImfChromaticitiesAttribute.h>
#include <OpenEXR/ImfDeepFrameBuffer.h>
#include <OpenEXR/ImfDeepScanLineInputFile.h>
#include <OpenEXR/ImfFrameBuffer.h>
#include <OpenEXR/ImfInputFile.h>
#include <half.h>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <string>
#include <vector>

using namespace Imf;

static float
halfToFloat (half h)
{
    return static_cast<float> (h);
}

extern "C" int
siv_imf_deep_scanline_flatten_rgba (
    const char* path, float* out_rgba, std::size_t out_len, unsigned* out_w, unsigned* out_h)
{
    if (!path || !out_rgba || !out_w || !out_h)
        return -1;
    *out_w = 0;
    *out_h = 0;

    try
    {
        DeepScanLineInputFile file (path);

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

        Array2D<float*> pR;
        Array2D<float*> pG;
        Array2D<float*> pB;
        Array2D<half*> pAh;
        Array2D<float*> pAf;
        Array2D<float*> pZ;
        pR.resizeErase (height, width);
        pG.resizeErase (height, width);
        pB.resizeErase (height, width);
        pAh.resizeErase (height, width);
        pAf.resizeErase (height, width);
        pZ.resizeErase (height, width);

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

        addFloatChan ("R", pR);
        addFloatChan ("G", pG);
        addFloatChan ("B", pB);
        if (aStorage == AStorage::Half)
            addHalfChan ("A", pAh);
        else if (aStorage == AStorage::Float)
            addFloatChan ("A", pAf);
        if (hasZ)
            addFloatChan ("Z", pZ);

        file.setFrameBuffer (fb);
        file.readPixelSampleCounts (ymin, ymax);

        for (int y = 0; y < height; ++y)
        {
            for (int x = 0; x < width; ++x)
            {
                unsigned int n = sampleCount[y][x];
                pR[y][x] = n ? new float[n] : nullptr;
                pG[y][x] = n ? new float[n] : nullptr;
                pB[y][x] = n ? new float[n] : nullptr;
                pAh[y][x] = (n && aStorage == AStorage::Half) ? new half[n] : nullptr;
                pAf[y][x] = (n && aStorage == AStorage::Float) ? new float[n] : nullptr;
                pZ[y][x] = (n && hasZ) ? new float[n] : nullptr;
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

                float* r = pR[y][x];
                float* g = pG[y][x];
                float* b = pB[y][x];
                half* ah = pAh[y][x];
                float* af = pAf[y][x];
                float* z = pZ[y][x];

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

        for (int y = 0; y < height; ++y)
        {
            for (int x = 0; x < width; ++x)
            {
                delete[] pR[y][x];
                delete[] pG[y][x];
                delete[] pB[y][x];
                delete[] pAh[y][x];
                delete[] pAf[y][x];
                delete[] pZ[y][x];
            }
        }

        *out_w = static_cast<unsigned> (width);
        *out_h = static_cast<unsigned> (height);
        return 0;
    }
    catch (...)
    {
        return -2;
    }
}

extern "C" int
siv_imf_input_file_chromaticities_f32 (const char* path, float* out_rg_bw_xy)
{
    if (!path || !out_rg_bw_xy)
        return -1;
    try
    {
        InputFile file (path);
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
    catch (...)
    {
        return -3;
    }
}
