#include <avif/avif.h>

void siv_avif_decoder_decode_all_content(avifDecoder * decoder)
{
    if (decoder) {
        decoder->imageContentToDecode = AVIF_IMAGE_CONTENT_ALL;
    }
}
