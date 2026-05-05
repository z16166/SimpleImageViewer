#include <avif/avif.h>

void siv_avif_decoder_decode_all_content(avifDecoder * decoder)
{
    if (decoder) {
        decoder->imageContentToDecode = AVIF_IMAGE_CONTENT_ALL;
    }
}

void siv_avif_decoder_set_image_content_flags(avifDecoder * decoder, avifImageContentTypeFlags flags)
{
    if (decoder) {
        decoder->imageContentToDecode = flags;
    }
}

/* Same as C: after avifDecoderCreate(), set decoder->strictFlags for viewer-style leniency. */
void siv_avif_decoder_set_strict_flags(avifDecoder * decoder, avifStrictFlags flags)
{
    if (decoder) {
        decoder->strictFlags = flags;
    }
}

avifImage * siv_avif_decoder_get_image(avifDecoder * decoder)
{
    return decoder ? decoder->image : NULL;
}

int siv_avif_decoder_get_image_count(avifDecoder * decoder)
{
    return decoder ? decoder->imageCount : 0;
}

avifBool siv_avif_decoder_image_sequence_track_present(avifDecoder * decoder)
{
    return decoder ? decoder->imageSequenceTrackPresent : AVIF_FALSE;
}

void siv_avif_decoder_copy_image_timing(avifDecoder * decoder, avifImageTiming * outTiming)
{
    if (decoder && outTiming) {
        *outTiming = decoder->imageTiming;
    }
}
