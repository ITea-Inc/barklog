package app.barklog.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import uniffi.barklog_ffi.SoundKind

/** Токены из docs/DESIGN.md: нейтрали graphite, один акцент, цвета классов. */
@Immutable
data class Palette(
    val bg: Color,
    val card: Color,
    val raised: Color,
    val line: Color,
    val text: Color,
    val text2: Color,
    val muted: Color,
    val bark: Color,
    val whine: Color,
    val howl: Color,
    val growl: Color,
    val other: Color,
    val silence: Color,
    /** Заливка главной кнопки и текст на ней. */
    val primary: Color,
    val onPrimary: Color,
    val danger: Color,
) {
    fun of(kind: SoundKind): Color = when (kind) {
        SoundKind.BARK -> bark
        SoundKind.WHINE -> whine
        SoundKind.HOWL -> howl
        SoundKind.GROWL -> growl
        SoundKind.OTHER -> other
        SoundKind.SILENCE -> silence
    }
}

private val Light = Palette(
    bg = Color(0xFFFAFAFA),
    card = Color(0xFFFFFFFF),
    raised = Color(0xFFF4F4F5),
    line = Color(0x14171717),
    text = Color(0xFF171717),
    text2 = Color(0xFF525252),
    muted = Color(0xFF737373),
    bark = Color(0xFFC27F22),
    whine = Color(0xFFC55A43),
    howl = Color(0xFF4F73B3),
    growl = Color(0xFF6B8448),
    other = Color(0xFFD4D4D8),
    silence = Color(0xFFECECEE),
    primary = Color(0xFF171717),
    onPrimary = Color(0xFFFFFFFF),
    danger = Color(0xFFDC2626),
)

private val Dark = Palette(
    bg = Color(0xFF141415),
    card = Color(0xFF19191B),
    raised = Color(0xFF232325),
    line = Color(0x14EDEDED),
    text = Color(0xFFEDEDED),
    text2 = Color(0xFFA1A1A6),
    muted = Color(0xFF85858B),
    bark = Color(0xFFD9973A),
    whine = Color(0xFFD8705A),
    howl = Color(0xFF7D9BD0),
    growl = Color(0xFF93A872),
    other = Color(0xFF3A3A3E),
    silence = Color(0xFF26262A),
    primary = Color(0xFFEDEDED),
    onPrimary = Color(0xFF141415),
    danger = Color(0xFFE5484D),
)

@Immutable
data class Type(
    val title: TextStyle,
    val subtitle: TextStyle,
    val heading: TextStyle,
    val body: TextStyle,
    val caption: TextStyle,
    val figure: TextStyle,
    val mono: TextStyle,
    val monoLarge: TextStyle,
)

private val Mono = FontFamily.Monospace
private const val TNUM = "tnum"

private fun type(p: Palette) = Type(
    title = TextStyle(fontSize = 28.sp, lineHeight = 32.sp, fontWeight = FontWeight.Medium, color = p.text),
    subtitle = TextStyle(fontSize = 16.sp, lineHeight = 24.sp, color = p.text2),
    heading = TextStyle(fontSize = 15.sp, lineHeight = 22.sp, fontWeight = FontWeight.Medium, color = p.text),
    body = TextStyle(fontSize = 15.sp, lineHeight = 22.sp, color = p.text),
    caption = TextStyle(fontSize = 13.sp, lineHeight = 18.sp, color = p.muted),
    figure = TextStyle(
        fontSize = 26.sp, lineHeight = 28.sp, fontWeight = FontWeight.Medium,
        fontFeatureSettings = TNUM, color = p.text,
    ),
    mono = TextStyle(fontFamily = Mono, fontSize = 12.sp, lineHeight = 16.sp, fontFeatureSettings = TNUM, color = p.text),
    monoLarge = TextStyle(fontFamily = Mono, fontSize = 16.sp, lineHeight = 22.sp, fontFeatureSettings = TNUM, color = p.text2),
)

val LocalPalette = staticCompositionLocalOf { Dark }
val LocalType = staticCompositionLocalOf { type(Dark) }

object Bark {
    val colors: Palette @Composable get() = LocalPalette.current
    val type: Type @Composable get() = LocalType.current
}

@Composable
fun BarkLogTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val p = if (dark) Dark else Light
    val scheme = if (dark) {
        darkColorScheme(
            primary = p.primary, onPrimary = p.onPrimary, background = p.bg, surface = p.card,
            onSurface = p.text, onSurfaceVariant = p.text2, surfaceVariant = p.raised,
            outline = p.line, error = p.danger, surfaceContainerHigh = p.raised,
        )
    } else {
        lightColorScheme(
            primary = p.primary, onPrimary = p.onPrimary, background = p.bg, surface = p.card,
            onSurface = p.text, onSurfaceVariant = p.text2, surfaceVariant = p.raised,
            outline = p.line, error = p.danger, surfaceContainerHigh = p.card,
        )
    }
    CompositionLocalProvider(LocalPalette provides p, LocalType provides type(p)) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}
